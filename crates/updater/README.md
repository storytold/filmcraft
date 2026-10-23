# filmcraft-updater

Self-update for the FilmCraft desktop app: find the newest release, download it, prove it is intact and
that it runs on this machine, swap it in after the app has exited, and roll back automatically if the new
version will not start. Pure Rust (rustls + RustCrypto TLS, flate2, tar), no `unsafe`, no background daemon.

Layer: L5 (native only). The app (L6) drives it; nothing below depends on it.

## Flow

```text
check()    GET github.com/storytold/filmcraft/releases/latest   (redirect → vX.Y.Z; no API, no token)
           GET …/releases/download/vX.Y.Z/SHA256SUMS.txt        (checksum of filmcraft-X.Y.Z-linux-<arch>.tar.gz)
stage()    stream the tarball to .part (size cap, HTTPS only, SHA-256 while streaming) → rename on match
           safe extraction (no .., absolute paths, links or zip bombs) → run `<new>/bin/filmcraft --version`
           write staged.json                                       (nothing installed has changed yet)
apply()    helper process, after the app exited: <name>.new → fsync; <name> → <name>.prev; <name>.new → <name>
on_boot()  every start: unconfirmed update? count the boot; after 2 unconfirmed boots the next start
           restores .prev (the bad build is kept as .broken)
confirm()  the new version has run healthily → .prev is deleted after 7 days (cleanup_old_backups)
```

Installs that something else owns (Flatpak, Snap, AppImage, Nix, `/usr/bin` packages) and unwritable
directories are never touched; [`Updater::install_kind`] says why.

## Trust model (read this)

GitHub publishes a `SHA256SUMS.txt` with every release, so integrity and authenticity currently rest on
**HTTPS to `github.com/storytold/filmcraft`** (pinned URL prefix, HTTPS-only including redirects, operating
system certificate store). That protects against corruption, captive portals and network attackers. It does
**not** protect against a compromised GitHub account or release: for that the maintainers would have to
publish a detached signature (e.g. minisign/Ed25519 over `SHA256SUMS.txt`) and ship the public key in the
binary. The release pipeline lives in `storytold/craft-common-actions`, so that is a change for the
maintainers; the verification step belongs in `net::fetch_release`.

## How the app uses it

* **Help ▸ Check for Updates…** (`help.checkForUpdates`) is the only thing that touches the network. It always
  makes a fresh request (an interval of zero), and when a newer release exists it **downloads and stages it
  straight away** (up to the 600 MB download cap and 800 MB extracted) without asking; only the swap waits for the person to press
  **Restart to apply** in the status bar.
* `--update-apply --pid <pid> --staged <dir> --target <exe> [-- <launch arguments...>]` is the helper mode that
  performs the swap after the editor has flushed its recovery journal and exited. The editor passes its own
  command line after `--`, and the helper restarts the (new or restored) build with it, so `--data-dir`,
  `--empty`, `--control`, project and media paths survive the update. The restart happens even if the swap
  failed, so the editor never just disappears.
* `on_boot()` runs before any window or project recovery; `confirm()` runs after two painted frames.

## Not done yet

* Applying updates on macOS and Windows (they report "download the installer"; the tarball path is Linux).
* An automatic background check and a setting for it. `check()` can already throttle to one request per interval
  using a state file, but nothing calls it that way yet, so `Availability::Throttled` is not reachable from the
  app today. Treat the throttle as a building block for that future check.
* Asking before the download starts (today the menu item stages the release immediately).
* Replacing anything but `filmcraft` and `filmcraft-cli`: desktop entry, icons and other files in the tarball are
  not installed by an update, so a release that needs a new data file would need this extended first.
* Disk-space pre-check (needs `statvfs`, i.e. `unsafe` or a new dependency); a full disk surfaces as an I/O error
  and the staging directory is deleted.
* Stall timeout per read; the download has an overall 20-minute limit instead.

## Tests

`cargo test -p filmcraft-updater` (offline: archive attacks, swap/rollback in temp dirs, download limits
against a local server). `cargo test -p filmcraft-updater --test live -- --ignored --nocapture` runs the whole
check → download → verify → extract → smoke-test flow against the real latest release.
