# filmcraft-publish

Publishing exported media to video platforms. YouTube is first. The design and the open questions
are in [storytold/filmcraft#2626](https://github.com/storytold/filmcraft/issues/2626).

## What is here

- `http`: the `Transport` trait every uploader sends through.
  - Responses come back whatever their status, and redirects are not followed, so YouTube's
    `308 Resume Incomplete` reaches the uploader.
  - Feature `net` adds `UreqTransport`, the HTTPS client. It is the same pure-Rust stack as
    `filmcraft-speech`'s `download` (ureq, rustls + RustCrypto, the operating system's certificate
    verifier).
  - The feature is off by default and in wasm builds.
- `youtube`: [YouTube Data API v3](https://developers.google.com/youtube/v3/docs/videos/insert)
  `videos.insert` over the
  [resumable upload protocol](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol).
  - `VideoMetadata::validate` applies YouTube's limits before anything is sent:
    - title: 1–100 characters
    - description: at most 5000 bytes
    - tags: at most 500 characters together, where quotes around a tag with spaces count, and so do
      the commas between tags
    - no `<` or `>` in any of them
  - Privacy defaults to `private`.
  - `Uploader::start` opens a session. The session address must be on the upload server's origin,
    because the access token goes with every request.
  - `Uploader::upload` sends the file in chunks of a multiple of 256 KiB (8 MiB by default).
    - Before the first chunk and after every failure, it asks the server how many bytes it has
      (`Content-Range: bytes */<total>`) and goes on from there. A dropped connection, a server
      error or an app restart therefore costs at most one chunk. The `UploadSession` is serde so the
      caller can keep it.
    - 5xx, 429, rate limits and lost connections are retried with exponential backoff (1 s … 64 s)
      or the server's `Retry-After` (at most 10 minutes). It gives up after 8 failures in a row.
  - Account and quota errors map to `PublishError` variants with messages a user can act on:
    - `Unauthorized`: refresh the token
    - `NoChannel`
    - `QuotaExceeded`
    - `UploadLimitExceeded`
    - `SessionExpired`
  - `Control` reports progress and waits between attempts. Either one can cancel.
    - Progress also moves while a chunk goes out (`Transport::send_watched`), so a slow
      connection doesn't look stuck for a whole chunk, and a cancel stops the chunk there.
    - `ThreadControl` sleeps in 100 ms steps and stops when a cancel flag is set.

- `oauth`: signing in with Google through
  [OAuth 2.0 for installed apps](https://developers.google.com/identity/protocols/oauth2/native-app).
  - `ClientConfig::from_google_json` reads the file Google Cloud downloads for a *Desktop app*
    client. A Web application client, a non-Google client id, or endpoints that aren't Google's
    are refused with a reason.
  - `SignIn::start` (feature `net`) creates a random state and a PKCE verifier (RFC 7636, `S256`)
    and listens on `127.0.0.1` on a free port. `SignIn::url` is the address to open in the
    browser. Only the challenge goes there, never the verifier or the client secret.
  - `Loopback::wait` waits for the browser's return, which it checks against the state. It
    answers other requests (`/favicon.ico`) with 404 and stops on cancel or timeout. The page the
    browser shows escapes everything that came from the request.
  - `SignIn::exchange` trades the code for tokens, and `refresh` gets a new access token.
    - A revoked or expired refresh token (`invalid_grant`) is `Unauthorized`: the user signs in
      again.
    - `Tokens::grants` tells whether the user left YouTube upload ticked on the consent page.

Error messages and `Debug` output never contain tokens, the client secret, the PKCE verifier or
the session address, which authorizes the upload.

## Not here yet

The engine's `publish.youtube.*` commands (sign-in and upload as jobs, the account kept in the
app's data folder) use this crate. Next is the Export page's *Publish to* section.

## Tests

- `src/youtube_tests.rs` runs the protocol against scripted server replies:
  - chunks, partial chunks, dropped connections, resume after a restart
  - `Retry-After`, backoff, giving up, stalls
  - every mapped error, hostile `Range` headers and response bodies
  - progress and cancel in the middle of a chunk
- `src/oauth_tests.rs` covers:
  - the RFC 7636 PKCE test vector
  - client files, good and refused
  - the browser address
  - callbacks: wrong state, denied, malformed, hostile
  - the token exchange and refresh form fields
  - every token error, hostile token answers, percent-encoding
- `tests/sign_in.rs` runs the whole sign-in on real sockets:
  - A fake browser follows the address to the loopback listener.
  - A fake token server checks the PKCE verifier against the challenge the browser carried.
  - It also covers denial, an escaped hostile answer, cancel and timeout.
- `tests/fake_server.rs` runs the real `UreqTransport` against a local fake upload server. The
  server cuts the connection in the middle of a chunk, answers `503 Retry-After: 1`, and a new
  uploader resumes from a saved session. Progress rises while one large chunk goes out, and a
  cancel stops that chunk before the server has it.

No test touches the network. The crate's dev-dependency on itself with `net` lets
`cargo test --workspace` and clippy build the transport.
