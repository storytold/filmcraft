# Releasing FilmCraft

Every push to the `release` branch runs `.github/workflows/release.yml`. The workflow builds
installers for macOS, Windows, Linux (AppImage, deb, rpm, tarball, Flatpak) and FreeBSD, plus
the web build, and creates or updates a **draft**
GitHub Release named `FilmCraft v<version>`. Nobody sees a draft until a maintainer publishes it.

User-facing names say **FilmCraft**. Files, binaries and ids stay lowercase
(`filmcraft-<version>-<platform>-<arch>.<ext>`, `ai.storyteller.filmcraft`).

## Cutting a release

1. **Bump the version** on `main`. It lives in exactly one place, `[workspace.package] version`
   in the root `Cargo.toml`; every crate inherits it and the packaging scripts read it from there:

   ```sh
   cargo xtask version                 # prints the current version, e.g. 0.2.1
   cargo xtask version set 0.3.0       # or 0.3.0-rc.1; updates Cargo.toml and Cargo.lock
   ```

   Commit the change (`Cargo.toml` + `Cargo.lock`) through the normal review flow, as a
   `Release: FilmCraft v0.3.0` commit whose message says what changed for users.
2. **Merge `main` into `release`** (or fast-forward it) and push. The workflow starts by itself.
3. **Wait for the draft.** When every job is done (macOS notarization is the slow part), the
   Releases page has a draft `FilmCraft v0.3.0`, targeting the pushed commit, with every artifact
   and `SHA256SUMS.txt`. The notes are generated from the merged pull requests.
4. **Check it.** Download an installer or two and read the job summaries. A `::warning::` there
   means a signing secret was missing and that artifact is unsigned.
5. **Publish** the draft in the GitHub UI. Publishing creates the `v0.3.0` tag. A version with a
   pre-release suffix (`-rc.1`) is marked as a pre-release.

Pushing to `release` again before you publish rebuilds the same draft and replaces its assets.
Once the draft is published, the workflow refuses to touch that version again: bump it first.

**Test runs:** *Actions › Release › Run workflow* runs the whole pipeline by hand. The optional
`version` input (such as `0.3.0-rc.1`) overrides `Cargo.toml` for that run only; each build job
applies it with `cargo xtask version set` before building. The signing jobs (macOS, Windows)
and the draft-release job run in the `release` environment, which only the `release` branch can
use, so pick that branch for a full run. Run from any other branch, it is a dry run: the jobs that
sign nothing (Linux, Flatpak, FreeBSD, web) build and upload their artifacts to the run, the
signing jobs are refused, and the draft-release job, which needs them, is skipped.

## What gets built

| Platform | Artifacts | Built on |
|---|---|---|
| macOS 11+ (universal: Apple silicon and Intel) | `filmcraft-<v>-macos-universal.dmg`, `filmcraft-cli-<v>-macos-universal.zip` | `macos-15` |
| Windows x64 | `filmcraft-<v>-windows-x64.msi`, `filmcraft-<v>-windows-x64-portable.zip` | `windows-latest` |
| Windows x86 (32-bit) | `filmcraft-<v>-windows-x86.msi`, `filmcraft-<v>-windows-x86-portable.zip` | `windows-latest` |
| Windows on ARM64 | `filmcraft-<v>-windows-arm64.msi`, `filmcraft-<v>-windows-arm64-portable.zip` | `windows-latest` (cross-compiled) |
| Linux x86_64 | `filmcraft-<v>-linux-x86_64.{AppImage,deb,rpm,tar.gz}` | `ubuntu-22.04` |
| Linux aarch64 | `filmcraft-<v>-linux-aarch64.{AppImage,deb,rpm,tar.gz}` | `ubuntu-22.04-arm` |
| AppImage updates | `filmcraft-<v>-linux-{x86_64,aarch64}.AppImage.zsync` | with the AppImage |
| Flatpak | `filmcraft-<v>-linux-x86_64.flatpak`, `filmcraft-<v>-linux-aarch64.flatpak` | `ubuntu-24.04`, `ubuntu-24.04-arm` (repackaged Linux tarball) |
| FreeBSD 14 x86_64 | `filmcraft-<v>-freebsd-x86_64.tar.gz` | FreeBSD 14.3 VM on `ubuntu-latest` |
| Web | `filmcraft-web-<v>.zip`, a static site (see [`packaging/web/README.md`](../packaging/web/README.md)) | `ubuntu-latest` |

`filmcraft --version` and `filmcraft-cli --version` print the version from `Cargo.toml`.

**Fonts.** Every build job checks out [craft-fonts](https://github.com/storytold/craft-fonts) at a
pinned commit (the `ref:` of its craft-fonts checkout step, marked `# bump deliberately`) and builds
with `CRAFT_FONTS_DIR` and `CRAFT_FONTS_REQUIRED=1`, so releases embed its Japanese fonts and fail
rather than ship without them (the web build embeds only the UI font, to stay small). The packages
carry each embedded font's licence (`copy_font_licences` in `packaging/env.sh`). Bump the pins
deliberately, in every job at once.

### macOS

`packaging/macos/package.sh` builds `aarch64-apple-darwin` and `x86_64-apple-darwin` with
`MACOSX_DEPLOYMENT_TARGET=11.0`, joins them with `lipo` and assembles `FilmCraft.app`:

- `Info.plist` is generated from `Info.plist.in` (bundle id `ai.storyteller.filmcraft`, the
  version and the build commit).
- **Signing** uses the hardened runtime and a secure timestamp, with the entitlements in
  `entitlements.plist`. The Developer ID certificate is imported into a temporary keychain by
  `packaging/macos/import-cert.sh`.
- **Notarization:** the app is sent with `xcrun notarytool submit`, then the ticket is stapled
  and checked with `stapler validate`. The app goes on a DMG (`hdiutil makehybrid`), which is
  signed and notarized too. The universal `filmcraft-cli` is signed the same way, zipped, and the zip is notarized.

Locally, without certificates, the script signs ad hoc and skips notarization, which is enough to
check the bundle and the DMG on your own Mac (`packaging/macos/package.sh`).

### Windows

`packaging/windows/package.ps1 -Arch x64|x86|arm64` builds with `+crt-static`, so neither the MSI
nor the portable zip needs the Visual C++ redistributable.

- `filmcraft.wxs` (WiX) is a per-machine install into Program Files with a Start Menu shortcut and
  an App Paths entry (Win+R `filmcraft`). Same-version upgrades are allowed, so release candidates
  replace each other.
- The ARM64 build is cross-compiled on the x64 runner. `.github/workflows/windows-arm64.yml`
  installs that MSI on a Windows 11 ARM64 runner, runs `filmcraft-cli --version` natively and
  uninstalls.
- **Signing:** `packaging/windows/sign.ps1` signs the executables and then the `.msi` with
  `signtool`, using whichever material is present: a `.pfx` certificate (`WINDOWS_CERTIFICATE`,
  base64, and `WINDOWS_CERTIFICATE_PASSWORD`) or Azure Trusted Signing (the `AZURE_*` secrets). With
  neither, it warns and leaves the files unsigned, so test builds still produce installers.

### Linux

`packaging/linux/package.sh` builds the release binaries and packages them as an AppImage, a
`.deb` and an `.rpm` (with [nfpm](https://nfpm.goreleaser.com), from `nfpm.yaml`) and a plain
`.tar.gz` tree. The packages install both programs, the desktop entry, the AppStream metainfo, the
icons and the licence files.

The jobs run on `ubuntu-22.04`, the oldest GitHub-hosted image, so the binaries only need
glibc 2.35 or newer: Ubuntu 22.04+, Debian 12+, Fedora 36+ and RHEL 10. The deb (`libc6 (>= 2.35)`)
and the rpm (`glibc >= 2.35`) both declare that floor, so older systems refuse the install. Moving
the job to a newer image raises the floor, so do it deliberately and update `nfpm.yaml` with it.
After packaging, the job prints the `.deb`'s metadata and contents, runs `ldd` on the binary and
runs each AppImage with `--version`.

**AppImage updates.** Each AppImage embeds update information,
`gh-releases-zsync|storytold|filmcraft|latest|filmcraft-*-linux-<arch>.AppImage.zsync`, and
`appimagetool` writes the matching `.zsync` beside it (the job installs `zsync` for
`zsyncmake`). [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) and
AppImageLauncher then fetch only the changed blocks from the newest published (non-pre-release)
release. The job checks both with `--appimage-updateinformation` and `test -s`.

**Flatpak.** The `flatpak` jobs (x86_64 on `ubuntu-24.04`, aarch64 on `ubuntu-24.04-arm`) run
`packaging/linux/flatpak-bundle.sh` on the Linux job's tarball: it installs the binaries with
`packaging/linux/flatpak/ai.storyteller.filmcraft.bundle.yml` into a single-file bundle,
`filmcraft-<v>-linux-<arch>.flatpak` (no Rust build, so it takes a minute), then installs it and
runs `filmcraft-cli --version` in the sandbox. Users install it with
`flatpak install --user <file>`; the freedesktop runtime comes from Flathub.
`packaging/linux/flatpak/ai.storyteller.filmcraft.yml` is the from-source manifest for a Flathub
submission (its header says how to build it). Both manifests must keep the same runtime and
`finish-args` (packaging-lint checks this).

### FreeBSD

GitHub has no FreeBSD runners, so the `freebsd` job builds in a FreeBSD 14.3 VM
([vmactions/freebsd-vm](https://github.com/vmactions/freebsd-vm), pinned by commit) on
`ubuntu-latest`. `packaging/freebsd/package.sh` builds both programs and packs a
`/usr/local`-style tree as `filmcraft-<v>-freebsd-x86_64.tar.gz`; install it with
`tar -xzf filmcraft-<v>-freebsd-x86_64.tar.gz --strip-components 1 -C /usr/local`. The build needs
the packages listed in the job (winit/wgpu, GTK 3 for the file dialogs, fontconfig, and
`alsa-lib` for audio through cpal). `.github/workflows/freebsd.yml` builds and packages the same
way nightly, so a FreeBSD break shows up before a release. `package.sh --dry-run` checks the
tree's layout with stub binaries on any OS (packaging-lint runs it).

### Web

The web job builds `apps/filmcraft-web` with `cargo xtask web` (the `wasm-bindgen` CLI must match
the crate's version exactly, `WASM_BINDGEN` in `xtask/src/main.rs`) and zips the static site with
`packaging/web/package.sh`. [`packaging/web/README.md`](../packaging/web/README.md) covers hosting.

## The draft release

The last job waits for every build, downloads their artifacts, writes `SHA256SUMS.txt` and creates
the draft `FilmCraft v<version>` with notes generated from the merged pull requests. If the draft
already exists, it replaces its assets and keeps it a draft. If that version is already published,
the job fails and asks for a version bump (`cargo xtask version set`).

## Secrets

The macOS, Windows and draft-release jobs run in the `release` environment, which only the
`release` branch can use and which holds the signing secrets. The other jobs sign nothing and get
no secrets. Every secret is optional: a missing one produces unsigned artifacts and a
warning, never a failed build.

| Secret | Used for |
|---|---|
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD` | the Developer ID certificate (base64 `.p12`), imported into a temporary keychain |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | notarization (`APPLE_PASSWORD` is an app-specific password) |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Windows signing with a `.pfx` |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE` | Windows signing with Azure Trusted Signing |
