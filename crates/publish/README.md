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
    `ThreadControl` sleeps in 100 ms steps and stops when a cancel flag is set.

Error messages and `Debug` output never contain the access token or the session address, which
authorizes the upload.

## Not here yet

These come next and use this crate:
- signing in: the OAuth installed-app flow with PKCE and a loopback redirect, plus token refresh
- token storage
- the `publish.*` engine commands, with the upload as a job
- the Export page's *Publish to* section

## Tests

- `src/youtube_tests.rs` runs the protocol against scripted server replies:
  - chunks, partial chunks, dropped connections, resume after a restart
  - `Retry-After`, backoff, giving up, stalls
  - every mapped error, hostile `Range` headers and response bodies, cancel
- `tests/fake_server.rs` runs the real `UreqTransport` against a local fake upload server. The
  server cuts the connection in the middle of a chunk, answers `503 Retry-After: 1`, and a new
  uploader resumes from a saved session.

No test touches the network. The crate's dev-dependency on itself with `net` lets
`cargo test --workspace` and clippy build the transport.
