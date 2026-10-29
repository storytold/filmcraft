# FilmCraft on the web

`apps/filmcraft-web` runs the same engine and egui UI as the desktop app in the browser: the
workspace compiled to `wasm32-unknown-unknown`, started by eframe's web runner on WebGPU (WebGL2
fallback). It is a static site: one `.wasm`, its `wasm-bindgen` JavaScript glue, `index.html`, an
`AudioWorklet` script and an icon. Nothing is uploaded anywhere; media stay on the user's machine.

## Build and run

| Tool | Version | Install |
|---|---|---|
| Rust target | `wasm32-unknown-unknown` | `rustup target add wasm32-unknown-unknown` |
| wasm-bindgen CLI | exactly the `wasm-bindgen` crate version (0.2.129) | `cargo install wasm-bindgen-cli --version 0.2.129 --locked` |
| wasm-opt (optional) | any recent binaryen | `brew install binaryen`; used by `cargo xtask web` when found |

```sh
cargo xtask web                 # release build → target/web/dist (index.html, filmcraft_web.js, filmcraft_web_bg.wasm, …)
cargo xtask web --dev           # unoptimised build (faster to compile, slow to run)
cargo xtask web --serve 8765    # build, then serve dist on http://127.0.0.1:8765/
```

`--serve` is a tiny localhost-only static server that sends `Cross-Origin-Opener-Policy` /
`Cross-Origin-Embedder-Policy`, so the page is cross-origin isolated like a production deployment
should be. Any static server works (`python3 -m http.server -d target/web/dist 8765`); it must
serve `.wasm` as `application/wasm` for streaming compilation.

The web crate is empty on non-wasm targets, so `cargo test --workspace` and clippy are
unaffected; `cargo xtask wasm` (part of `cargo xtask ci`) checks it, with `filmcraft-ui-egui`, for
`wasm32-unknown-unknown`.

URL flags: `?empty` (no demo project), `?norecover` (don't reopen the auto-saved project),
`?fresh` (also don't restore media kept in OPFS), `?cpu` (CPU compositor), `?webgl` (WebGL2 only;
the page reloads with it when WebGPU is present but fails to start), `?nowebcodecs` (decode with
FilmCraft's own decoders only).

If the app panics after start-up (or runs out of memory), `index.html` shows a "FilmCraft stopped
working" overlay with the panic message and a Reload button instead of a frozen canvas, and sets
`window.filmcraftLoad.fatal`. Code the web build runs must not call `std::env::temp_dir`,
`std::time::{Instant, SystemTime}::now`, `std::thread::sleep`/`spawn` or `std::process::id`: they
panic on `wasm32-unknown-unknown` (use `filmcraft_engine::temp_dir`, `web_time`, or a `cfg`).

## How the desktop pieces map to the browser

| Desktop | Web |
|---|---|
| frame worker threads | `FrameServer::pump`: queued frame jobs run on the UI thread between egui frames, highest priority first, within a time budget (24 ms while playing, 40 ms otherwise) |
| export on a worker thread | `filmcraft_export::Exporter`: the export advances one frame per step from `Session::pump_jobs`, 30 ms per UI frame; progress shows in the header like on the desktop |
| `std::fs` (`FsServices`) | `fs::WebServices`: a virtual file table. Picked/dropped files are `File` handles (never copied); files the app writes are kept in memory and offered as downloads |
| file dialogs | File System Access `showOpenFilePicker` where available, else `<input type=file>`; drop files anywhere on the page |
| crash-recovery journal | OPFS: `recovery/snapshot.fcproj` every 5 s while there are unsaved changes, plus copies of imported media in `media/`; the next visit reopens the snapshot |
| cpal output | WebAudio `AudioWorklet` |
| VideoToolbox & co. | WebCodecs `VideoDecoder` |
| TCP control channel / MCP | `window.filmcraft` (below) |

### Threads

wasm threads need a cross-origin isolated page **and** a wasm build with atomics (a nightly
`build-std` toolchain), so this build is single-threaded everywhere: frame rendering, decoding,
mixing and encoding are cooperative on the UI thread, and `rayon` runs inline.
`filmcraft.info()` reports `crossOriginIsolated` so a threaded build can choose at start-up.
Commands that need a thread on the desktop (render previews, proxies, Project Manager, mask
tracking) report an error on the web for now.

### Media reads

Containers are opened through `Services::reader` (a `filmcraft_media::ByteReader`): MP4/MOV and
Matroska read their index at import and single samples afterwards, so a multi-gigabyte file is
never loaded whole. Browser `Blob` reads are asynchronous, so `BlobReader` keeps a 384 MiB LRU cache
of 1 MiB chunks: a read whose chunks are missing starts fetching them (plus three chunks of
read-ahead) and fails with `WouldBlock`, setting `filmcraft_media::pending`. The frame server then
drops that frame's result and retries the job after the bytes arrive (the fetch wakes the UI);
the stepped exporter re-renders the frame; an import retries `file.import`. Before importing, the
container index is prefetched (an MP4's `moov` box wherever it is, a small Matroska file whole).
Stills, WAV and compressed-audio files are decoded whole, as on the desktop.

Limits: Matroska/WebM files larger than the chunk cache import slowly (opening scans every
cluster header); a file the user picked is only readable while the page is open, unless its OPFS
copy exists (files up to 4 GiB are copied in the background).

### Decoding

FilmCraft's own decoders (H.264, HEVC, VP9, AV1, ProRes, DNxHR, MJPEG, AAC, Opus…) run in the
browser unchanged. When the browser has WebCodecs, H.264, HEVC, VP9 and AV1 video in MP4/MOV is
decoded by the browser's (usually hardware) `VideoDecoder` instead: `webcodecs::reader_opener` is
registered ahead of the built-in openers and opens such files as a `WcSource`, whose audio and
media info come from our `Mp4Source`. WebCodecs is asynchronous, so a frame request feeds samples
to the decoder and returns "pending"; the decoder's output callback converts the `VideoFrame` to
RGBA (drawn on an `OffscreenCanvas`) into a per-source frame cache, and the retried request finds
it. Codec support is probed at start-up with `VideoDecoder.isConfigSupported`; any decoder error
switches that source to our decoder.

### Audio

`audio::Handle` implements `AudioOut`. The UI thread mixes the sequence 250 ms ahead in 2048-frame
blocks and posts them to the worklet (`web/audio-worklet.js`), which counts the frames it really
played and reports them with its `currentTime`. `played_frames` extrapolates from the last report
on the audio context's clock, never past what was queued — so the audio clock is the playback
master as on the desktop, and a starved worklet holds the playhead. Browsers start audio suspended
until the first click or key press; until then playback runs on the wall clock.

### GPU

eframe creates a WebGPU device when the browser has one, else WebGL2. FilmCraft's GPU compositor
(`filmcraft-gpu`) is used on WebGPU; on WebGL2 frames are composited on the CPU. Check
`filmcraft.info().backend` and `.compositor`.

## Remote storage

The page can mount folders of a WebDAV server as virtual paths, so projects and media live on a
server instead of being picked from the local disk. Without the config file below nothing
changes: the app behaves as before and no remote menu entries appear.

Put `filmcraft-config.json` next to `index.html`:

```json
{"remote":{"mounts":[{"path":"/data","url":"dav/","name":"Server","writable":false}],"listCacheSeconds":15}}
```

| Field | |
|---|---|
| `path` | Virtual path of the mount (`/data`). Absolute, no `..`; it may not be `/files`, `/projects`, `/exports`, `/opfs` or `/recovered`, and mounts may not overlap. |
| `url` | The WebDAV collection: relative to the page (`dav/`), a path on the same site (`/dav/`) or an absolute URL on the page's own origin. Other origins are refused (no query-string override either). |
| `name` | Name in the Media Browser (default: the last segment of `path`). |
| `writable` | `true` allows saving to the mount. Default `false`: read-only. |
| `remote.chunkBytes` | Optional, next to `mounts`: size of one `Range` read of remote media (bytes, default `4194304`, clamped to 262144 to 16777216). Local files stay at 1 MiB. Not a number: the default. |
| `remote.parallel` | Optional, next to `mounts`: how many chunks of a remote file are fetched at once while it is scanned (default `12`, clamped to 1 to 32). Not a number: the default. |
| `remote.listCacheSeconds` | Optional, next to `mounts`: how long a folder listing is reused before the server is asked again (seconds, default `15`, clamped to 0 to 3600; `0` revalidates every time the folder is refreshed). Not a number: the default. |

At most 16 mounts. A missing, invalid or non-JSON file (a single-page app that answers 200 with
its index page, for example) turns the feature off and logs one line.

What it gives you:

- Each mount is a **Network** location in the Media Browser. Listings are WebDAV `PROPFIND`
  (Depth 1), cached for `listCacheSeconds` (15 by default) and refreshed in the background; a folder that is still
  loading shows "Loading…" and fills in by itself.
- Media is read with `Range` requests through the same chunk cache as local files (4 MiB chunks,
  see `remote.chunkBytes`). Reads that miss at a frontier moving forward, such as a Matroska open
  scanning every cluster header, fetch `remote.parallel` chunks ahead instead of three, and the
  retries of an import run again as soon as a chunk arrives. Opening a remote project also loads the
  container index of its WebM, Matroska and QuickTime media, and the media of the active sequence
  (the clips under its playhead first) is fetched first: other sequences' media starts when it is
  loaded, and meanwhile other files, such as Project panel thumbnails, fetch only a little. The server
  must answer `206 Partial Content` with a matching `Content-Range`; a `200` full body is refused
  for media.
- **File ▸ Open Remote Project…**, **Import from Remote…** and **Save As Remote…** (and the Home
  cards and the Project panel menu) open the remote dialog. The local entries are then labelled
  "Local". Right-click a project in the Media Browser for **Open Project…**; double-click is
  unchanged.
- A project opened from a mount first fetches the sizes of the media it refers to (a listing of
  each folder, `HEAD` as fallback), because the engine's file services are synchronous. Media
  the server does not list show as offline. `filmcraft.openProject("/data/a.fcproj")` does the
  same from JavaScript. A recovered snapshot re-checks its remote media the same way.
- Everything a server sends is treated as untrusted: capped body and listing sizes, hrefs outside
  the mount dropped, malformed XML, ranges and sizes refused.

### Saving

Only a mount with `"writable": true` takes writes; saving to a read-only mount fails with a message
(use Save As to another place). Saving is a background upload:

1. the project is uploaded to `<name>.fcproj.part` in the same folder (the old file stays whole);
2. the target's ETag is compared with the one seen when the project was opened (or last saved).
   If another editor changed it meanwhile (for example the headless MCP on the same project),
   nothing is overwritten: the status line says "Project changed on the server; use Save As" and
   the project stays unsaved;
3. the `.part` file is moved over the target (`MOVE`, `Overwrite: T`; `Overwrite: F` when the
   file is new, so an existing file is never replaced silently).

Any other failure also leaves the project unsaved and shows the reason; the `.part` file is
deleted on a best-effort basis. The ETag check is a `HEAD` before the `MOVE`: WebDAV has no atomic
compare-and-move, so a change in the instant between the two is not caught. A server that sends
no ETag cannot be checked. After the move the saved version is recorded from the server's ETag; when
the server gave the moved file a new ETag (some servers derive it from the modification time), the
file is read back and the new ETag is recorded only if its bytes are the ones just uploaded, so
the next save of the session goes ahead while a file another client wrote stays a conflict.

### Server setup

Any WebDAV server on the page's origin works if it supports `PROPFIND`, `Range` GETs and (for
writable mounts) `PUT`, `MOVE` and ETags. Serve the app and the DAV folder from one origin, for
example with nginx in front:

```nginx
location /filmcraft/ { root /srv/www; }                    # the static app + filmcraft-config.json
location /filmcraft/dav/ {
    proxy_pass http://127.0.0.1:8080;                      # a WebDAV server; no URI here, the prefix is kept
    proxy_set_header Host $http_host;                      # the Destination of a MOVE names this host
    client_max_body_size 0;                                # saved projects, no limit
    proxy_request_buffering off;
}
```

with, for example, `rclone serve webdav /srv/films --addr 127.0.0.1:8080 --baseurl /filmcraft/dav`
(add `--read-only` for a read-only mount), or Apache with `mod_dav` and `mod_dav_fs` (`Dav On` on
the folder). The server's own access control applies; the browser sends the page's cookies and HTTP
authentication as for any same-origin request.

Three rules keep saving working:

- The proxy must not strip the location prefix: write `proxy_pass http://127.0.0.1:8080;` without
  a trailing `/` or path. With `proxy_pass http://…:8080/;` the server sees paths without the
  prefix, but the `Destination` header of the `MOVE` that finishes a save still carries it, so the
  save fails.
- `rclone --baseurl` equals the location prefix (`/filmcraft/dav`), and the mount's `url` is that
  same prefix: relative to the page (`dav/`) or absolute (`/filmcraft/dav/`).
- `--read-only` on the server goes with `"writable": false` in the config, and the other way round.

## `window.filmcraft`: the agent / test API

The control channel of the desktop app (`docs/control-protocol.md`) as promises. Results resolve
with the method's `result`, errors reject.

| Call | |
|---|---|
| `filmcraft.execute(command, params)` | run an engine command (same ids and params as MCP / `filmcraft-cli`) |
| `filmcraft.request(method, params)` | any control method: `ui.inspect`, `ui.click`, `ui.key`, `ui.playback`, `ui.menu.invoke`… |
| `filmcraft.commands()` | the command list |
| `filmcraft.inspect()` | `ui.inspect` |
| `filmcraft.screenshot(params)` | `{pngBase64, width, height}` of the canvas (or `{panel}`) |
| `filmcraft.importFiles(files)` | import `File` / `FileList` / `File[]`; resolves `{items, errors, paths}` |
| `filmcraft.importUrl(url, name?)` | fetch a URL and import it (tests) |
| `filmcraft.openProject(fileOrUrl)` | open a `.fcproj`: a File, a URL, or a path on a remote mount (see [Remote storage](#remote-storage)) |
| `filmcraft.files()` | the virtual files: `{path, size, kind}` |
| `filmcraft.readFile(path)` | a file's bytes (`Uint8Array`), e.g. an export |
| `filmcraft.info()` | backend, compositor, isolation, OPFS, WebCodecs support and counters, start-up time |

`window.filmcraftLoad` holds `{wasmMs, readyMs}` (module fetch+compile, and until the app runs).

```js
await filmcraft.importUrl("/clip.mp4");
await filmcraft.execute("file.newSequence", {fromItem: 48});
await filmcraft.request("ui.playback", {action: "play"});
const {job} = await filmcraft.execute("file.exportMedia", {format: "h264", path: "/exports/out.mp4"});
```

## Browser test

`apps/filmcraft-web/tests/smoke.mjs` drives headless Chrome over the DevTools protocol (Node ≥ 22,
no npm packages): load, demo playback, import of a generated MP4, playback, H.264 export
(download). It writes screenshots and `report.json`:

```sh
cargo xtask web --serve 8765 &
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=30 -f lavfi -i sine -t 3 -c:v libx264 -pix_fmt yuv420p -c:a aac target/web/dist/web-test.mp4
node apps/filmcraft-web/tests/smoke.mjs --url http://127.0.0.1:8765/ --media web-test.mp4 --out target/web/smoke
```
