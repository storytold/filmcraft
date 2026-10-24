# Windows installer UI

`installer-ui.wxs` is generated from the shared `craft-installer` crate in
[craft-libs](https://github.com/storytold/craft-libs/tree/a3d56cbb41f3e86db4c1a1114d71d56009de8461/crates/installer).
`package.ps1` compiles it with this app's package source. The common UI provides progress,
repair/remove, and explicit install/uninstall/repair success, cancellation and failure screens.

Regenerate or check from a sibling craft-libs checkout at
`a3d56cbb41f3e86db4c1a1114d71d56009de8461`:

```sh
CARGO_BUILD_JOBS=1 cargo run -j1 --locked -p craft-installer -- --write --app filmcraft ../filmcraft/packaging/windows/installer-ui.wxs
CARGO_BUILD_JOBS=1 cargo run -j1 --locked -p craft-installer -- --check --app filmcraft ../filmcraft/packaging/windows/installer-ui.wxs
```

Edit the shared templates/profile and regenerate; avoid editing this snapshot by hand. The
profile retains this app's existing folder, shortcut and launch options; defaults, persistence,
components, associations and signing stay in the app-owned source. Release builds require no
sibling checkout, published crate or additional runtime dependency. `/qn`, `/qb` and `/passive`
remain unattended and do not launch the app.

### Published shared-template correction

The pinned generator includes the Restart Manager correction in
[craft-libs #181](https://github.com/storytold/craft-libs/pull/181), a two-file follow-up stacked
on the shared installer feature in [craft-libs #157](https://github.com/storytold/craft-libs/pull/157).
Both PRs must land for the correction to reach craft-libs main. FilmCraft builds use this
checked-in snapshot; regeneration and drift checking use the exact commit above without
additional template edits.

The `MsiRMFilesInUse` OK button publishes `RMShutdownAndRestart` with value `0` and order `1`,
followed by `EndDialog` / `Return` with order `2`.

Microsoft documents that [MSIRMSHUTDOWN=2](https://learn.microsoft.com/en-us/windows/win32/msi/msirmshutdown)
does not close any affected processes if one is not registered for restart. The
[documented dialog event](https://learn.microsoft.com/en-us/windows/win32/msi/msirmfilesinuse-dialog)
requests shutdown and restart through Restart Manager; policy and application behavior can
still prevent it. Cancel continues to exit setup. Run
`python3 packaging/windows/test_installer_ui.py` for the source regression checks; native
Windows files-in-use acceptance remains pending.

Historical shared-template checks: source/profile checks and native fixture MSI linking/UI table checks passed for x64, x86 and
ARM64. All 14 profiles passed full UI install, missing-file repair and cached uninstall, and
x86 silent install/removal. Folder upgrades, desktop choices and all six launch profiles also
passed their Windows fixture checks. See the
[Windows verification record](https://github.com/storytold/craft-libs/blob/a3d56cbb41f3e86db4c1a1114d71d56009de8461/docs/windows-installer-verification.md) for
interactive results, scope validation and remaining acceptance checks with real release payloads.
The shared crate includes reusable linking, full UI, option/upgrade/launch and x86 test runners.
Windows Settings now uses a separate MSI-owned entry whose uninstall command selects full UI,
including the completion screen. Its quiet command and direct `/qn`, `/qb`, `/passive` commands
remain unattended. The main feature references `CraftInstallerSettings`; the shared snapshot
owns this registration and hides the native MSI entry while retaining MSI repair/upgrade APIs.

The Settings display name appends `(Craft Apps)` for compatibility with older Launchers; the
hidden native MSI name stays unchanged. Existing ARP Modify policy is retained explicitly.

Those historical fixture results do not qualify the current FilmCraft payload. After rebasing,
native Windows qualification must be repeated on the exact PR head: build and validate the MSI,
install and upgrade, repair, uninstall from Settings through Finish, and install/remove with
each of `/qn`, `/qb` and `/passive`. Record the head, package hash, commands and results;
verify documents and settings survive removal. These current-head native checks remain pending.
