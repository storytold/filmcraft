$ErrorActionPreference = 'Stop'

$repo = 'C:\Users\Balázs Ferenczi\filmcraft-repo\filmcraft'
$expectedRemote = 'https://github.com/TheMiningPickaxe/filmcraft'
$title = 'Timeline Slider Overhaul Part #1'
$body = @'
FIRST COMMIT WHOHOOOOOO! IT'S BEEN SO FUN VIBE CODING!! I CAN'T WAIT TO DO MORE!

Known issues:

- The Setting Sliders are not correctly centered.
- The frame the menu opens, it glitches out then goes back to normal.
- When the sliders are set on Custom, the window jolts.
- The value: [Number] is not accurate the frame the sliders are set onto it, then it fixes itself.
- Auto mode for the Slow Jolt mode has not tested or adjusted to be perfectly satisfying. For example, Simply dragging the slider when it's zoomed in all the way, it's still too fast. For a v1 it's good enough though.

I'll fix these in future commits!

GPT-6 Luna Report:

Timeline zoom controls
- Added a right-click settings menu to the Timeline zoom slider.
- Added Smooth Animations to control whether double-click fit and playhead-centering zoom animate or snap immediately.
- Added double-click to fit the full Timeline extent.
- Added triple-click to center the playhead with a configurable amount of visible footage on each side.
- Added preset centering ranges of 3, 5, 10, and 30 seconds per side, plus a custom numeric value.
- Fixed playhead centering to use the full slider width, including the area over the volume-meter side.

Shift-drag sensitivity
- Added Shift-drag speed presets: Auto, 10%, 25%, and 50%, plus a custom value from 0.1% to 100%.
- Added exponential Auto sensitivity based on the visible Timeline duration: 50% at full view, 7.5% at 30 seconds, 2% at 10 seconds, and 0.1% at one frame or closer.
- Set Auto as the default for new projects.
- Renamed the setting to “Slow Drag Speed (Shift + Left Click).”

Settings persistence and menu behavior
- Stored Timeline interaction settings with the project view and recovery snapshots, so they persist with project saves and can be restored after a crash.
- Improved custom numeric fields to accept numeric input while retaining click-and-drag adjustment.
- Kept menu size stable while custom fields appear and disappear, and made repeated right-clicks reposition the menu.
- Made menu sliders larger and centered within the compact settings menu.

Verification
- Formatting and whitespace checks passed.
- Release build and app launch were previously verified using the existing D:\Other\FilmCraft Cargo target directory.
'@

if ((Get-Location).Path -ne $repo) {
    throw "Run this script from the FilmCraft repository folder: $repo"
}

$branch = (& git branch --show-current).Trim()
if ($branch -ne 'main') {
    throw "Expected branch 'main'; current branch is '$branch'. No files were committed or pushed."
}

$remote = (& git remote get-url origin).Trim().TrimEnd('/') -replace '\.git$', ''
if ($remote -ne $expectedRemote) {
    throw "The origin remote is '$remote', not '$expectedRemote'. No files were committed or pushed."
}

$staged = @(& git diff --cached --name-only)
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect staged files.' }
if ($staged.Count -eq 0) {
    throw 'There are no staged changes. Stage the Timeline files first; this script will not stage files automatically.'
}
if ($staged -contains 'AGENTS.md') {
    throw 'AGENTS.md is staged. Unstage it before publishing; this script will not commit it.'
}

Write-Host 'Staged files to be committed:'
$staged | ForEach-Object { Write-Host "  $_" }
Write-Host ''
Write-Host "Commit title: $title"
Write-Host ''
$confirmation = Read-Host 'Type YES to commit these staged files and push main to your GitHub fork'
if ($confirmation -cne 'YES') {
    Write-Host 'Cancelled. Nothing was committed or pushed.'
    exit 0
}

& git commit -m $title -F - <<'COMMIT_BODY'
$body
COMMIT_BODY
if ($LASTEXITCODE -ne 0) { throw 'Commit failed; nothing was pushed.' }

& git push origin main
if ($LASTEXITCODE -ne 0) { throw 'Commit succeeded, but pushing failed. The commit is still local.' }

Write-Host ''
Write-Host 'Published to https://github.com/TheMiningPickaxe/filmcraft/tree/main'
