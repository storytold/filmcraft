param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedPushDestination
)

$ErrorActionPreference = 'Stop'

$repo = $PSScriptRoot
$expectedRemote = $ExpectedPushDestination
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

Implementation summary:

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
- Release build and app launch were previously verified using a separate Cargo target directory.
'@

function Assert-NoUnstagedChanges {
    $unstaged = @(& git diff --name-only)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect unstaged changes. Nothing was committed or pushed.' }
    $untracked = @(& git ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect untracked files. Nothing was committed or pushed.' }
    if (($unstaged.Count + $untracked.Count) -gt 0) {
        throw 'Unstaged or untracked changes exist. Stage or remove them, then review the staged tree before publishing.'
    }
}

function Get-ValidatedPushUrls {
    $urls = @(& git remote get-url --push --all origin 2>$null)
    if ($LASTEXITCODE -ne 0 -or $urls.Count -eq 0) {
        throw 'Could not inspect origin push destinations. Nothing was committed or pushed.'
    }
    foreach ($url in $urls) {
        if ((Normalize-Remote $url) -cne (Normalize-Remote $expectedRemote)) {
            throw 'An origin push destination does not match the expected destination. Nothing was committed or pushed.'
        }
    }
    return ,$urls
}

function Normalize-Remote([string]$url) {
    if ([string]::IsNullOrEmpty($url)) {
        throw 'A push destination must be a valid GitHub repository URL.'
    }

    $owner = $null
    $repository = $null
    if ($url -match '^git@github\.com:(?<path>[^/?#]+/[^/?#]+?)(?:\.git)?/?$') {
        $parts = $Matches.path.Split('/')
        $owner = $parts[0]
        $repository = $parts[1]
    } else {
        $uri = $null
        if (-not [Uri]::TryCreate($url, [UriKind]::Absolute, [ref]$uri) -or $uri.Host -ine 'github.com' -or -not $uri.IsDefaultPort -or $uri.Query -or $uri.Fragment -or $uri.Scheme -notin @('https', 'ssh')) {
            throw 'A push destination must use HTTPS, SSH, or GitHub SCP syntax for github.com.'
        }
        $parts = $uri.AbsolutePath.Trim('/').Split('/')
        if ($parts.Count -ne 2) {
            throw 'A push destination must identify exactly one GitHub owner and repository.'
        }
        $owner = [Uri]::UnescapeDataString($parts[0])
        $repository = [Uri]::UnescapeDataString($parts[1])
    }

    if ($repository.EndsWith('.git', [StringComparison]::OrdinalIgnoreCase)) {
        $repository = $repository.Substring(0, $repository.Length - 4)
    }
    foreach ($part in @($owner, $repository)) {
        if ([string]::IsNullOrEmpty($part) -or $part -in @('.', '..') -or $part -notmatch '^[A-Za-z0-9_.-]+$') {
            throw 'A push destination must identify a valid GitHub owner and repository.'
        }
    }
    return "github.com/$owner/$repository".ToLowerInvariant()
}

function Assert-NoUnfinishedMerge {
    $mergeHeadPath = & git rev-parse --git-path MERGE_HEAD 2>$null
    if ($LASTEXITCODE -ne 0 -or $null -eq $mergeHeadPath) {
        throw 'Could not check whether a merge is in progress. Nothing was committed or pushed.'
    }
    if (Test-Path -LiteralPath ([string]$mergeHeadPath)) {
        throw 'An unfinished merge is in progress. Finish or abort it before publishing; nothing was committed or pushed.'
    }
}

Push-Location -LiteralPath $repo
try {
$branchOutput = & git branch --show-current 2>$null
if ($LASTEXITCODE -ne 0 -or $null -eq $branchOutput) {
	throw 'Could not identify the current branch. No files were committed or pushed.'
}
$branch = [string]$branchOutput
$branch = $branch -replace "`r?`n$", ''
if ([string]::IsNullOrWhiteSpace($branch) -or $branch -in @('main', 'master')) {
	throw 'Publish from a named feature branch, not main or master. No files were committed or pushed.'
}
$pushUrlsBeforeConfirmation = Get-ValidatedPushUrls
Assert-NoUnfinishedMerge

Assert-NoUnstagedChanges
$headBeforeConfirmation = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not identify HEAD before confirmation.' }
$stagedTreeBeforeConfirmation = (& git write-tree).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not record the staged tree.' }
$staged = @(& git -c core.quotePath=false diff-tree --no-commit-id --no-renames --name-only -r $headBeforeConfirmation $stagedTreeBeforeConfirmation)
if ($LASTEXITCODE -ne 0) { throw 'Could not list changes from the recorded staged tree.' }
if ($staged.Count -eq 0) {
    throw 'There are no staged changes. Stage the Timeline files first; this script will not stage files automatically.'
}
if (@($staged | Where-Object { $_ -match '(^|/)AGENTS\.md"?$' }).Count -gt 0) {
    throw 'AGENTS.md is staged. Unstage it before publishing; this script will not commit it.'
}

Write-Host 'Staged files to be committed:'
$staged | ForEach-Object { Write-Host "  $_" }
Write-Host ''
Write-Host "Commit title: $title"
Write-Host ''
$confirmation = Read-Host "Type YES to commit these staged files and push $branch to your GitHub fork"
if ($confirmation -cne 'YES') {
    Write-Host 'Cancelled. Nothing was committed or pushed.'
    exit 0
}

$headNow = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $headNow -ne $headBeforeConfirmation) {
    throw 'HEAD changed after confirmation. Review the repository before committing; nothing was pushed.'
}
$branchNowOutput = & git branch --show-current 2>$null
if ($LASTEXITCODE -ne 0 -or $null -eq $branchNowOutput) {
    throw 'Could not recheck the current branch after confirmation. Nothing was pushed.'
}
$branchNow = ([string]$branchNowOutput) -replace "`r?`n$", ''
if ($LASTEXITCODE -ne 0 -or $branchNow -cne $branch) {
    throw 'The current branch changed after confirmation. Review the repository before committing; nothing was pushed.'
}
$treeNow = (& git write-tree).Trim()
if ($LASTEXITCODE -ne 0 -or $treeNow -ne $stagedTreeBeforeConfirmation) {
    throw 'The staged tree changed after confirmation. Review the repository before committing; nothing was pushed.'
}
Assert-NoUnstagedChanges
$pushUrlsNow = Get-ValidatedPushUrls
if ((@($pushUrlsNow) -join "`n") -cne (@($pushUrlsBeforeConfirmation) -join "`n")) {
    throw 'Origin push destinations changed after confirmation. Review the repository before committing; nothing was pushed.'
}
Assert-NoUnfinishedMerge

$commitBodyPath = Join-Path ([System.IO.Path]::GetTempPath()) ("filmcraft-commit-{0}.txt" -f [guid]::NewGuid())
try {
    $commitMessage = "$title`n`n$body"
    [System.IO.File]::WriteAllText($commitBodyPath, $commitMessage, [System.Text.UTF8Encoding]::new($false))
    & git commit --file $commitBodyPath
    if ($LASTEXITCODE -ne 0) { throw 'Commit failed; nothing was pushed.' }
} finally {
    Remove-Item -LiteralPath $commitBodyPath -Force -ErrorAction SilentlyContinue
}

$commitId = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not identify the new commit; nothing was pushed.' }
$committedTree = (& git rev-parse "$commitId^{tree}").Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the new commit tree; nothing was pushed.' }
$parentsOutput = & git show --no-show-signature -s --format=%P $commitId 2>$null
if ($LASTEXITCODE -ne 0 -or $null -eq $parentsOutput) {
    throw 'Could not verify the new commit parents. Review the local commit; nothing was pushed.'
}
$parents = ([string]$parentsOutput).Split(' ', [StringSplitOptions]::RemoveEmptyEntries)
if ($committedTree -ne $stagedTreeBeforeConfirmation -or $parents.Count -ne 1 -or $parents[0] -cne $headBeforeConfirmation) {
    throw 'The commit does not match the confirmed tree and parent. Review the local commit; nothing was pushed.'
}

$pushUrlsBeforePush = Get-ValidatedPushUrls
if ((@($pushUrlsBeforePush) -join "`n") -cne (@($pushUrlsBeforeConfirmation) -join "`n")) {
    throw 'Origin push destinations changed before push. The commit is local; nothing was pushed.'
}
$script:PSNativeCommandUseErrorActionPreference = $false
& git push --no-follow-tags --no-recurse-submodules origin "$($commitId):refs/heads/$branch"
if ($LASTEXITCODE -ne 0) { throw 'Commit succeeded, but the push returned an error. A configured fallback destination may already have updated the fork branch; check the branch before retrying.' }

Write-Host ''
Write-Host "Published the confirmed commit to the validated destination branch '$branch'."
} finally {
    Pop-Location
}
