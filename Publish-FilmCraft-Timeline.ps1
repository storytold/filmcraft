param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedPushDestination,

    [string]$CommitMessage
)

$ErrorActionPreference = 'Stop'
$repo = $PSScriptRoot

function Normalize-GitHubRemote([string]$value) {
    if ([string]::IsNullOrWhiteSpace($value)) { throw 'The expected push destination is invalid.' }

    $path = $null
    if ($value -cmatch '^git@github\.com:(?<path>[^?#]+)$') {
        $path = $Matches.path
    } elseif ($value -match '^https://|^ssh://') {
        $uri = $null
        if (-not [Uri]::TryCreate($value, [UriKind]::Absolute, [ref]$uri)) { throw 'The expected push destination is invalid.' }
        if ($uri.Host -ine 'github.com' -or $uri.Query -or $uri.Fragment) {
            throw 'The expected push destination must be one GitHub repository URL.'
        }
        if (($uri.Scheme -eq 'https' -and $uri.Port -ne 443) -or ($uri.Scheme -eq 'ssh' -and $uri.Port -ne 22)) {
            throw 'The expected push destination uses an unsupported port.'
        }
        if ($uri.Scheme -eq 'https' -and $uri.UserInfo) { throw 'Use Git credential management instead of credentials in the push URL.' }
        if ($uri.Scheme -eq 'ssh' -and $uri.UserInfo -notin @('', 'git')) { throw 'The expected push destination must use the GitHub SSH account.' }
        if ($uri.Scheme -notin @('https', 'ssh')) { throw 'The expected push destination must use HTTPS or SSH.' }
        $path = $uri.AbsolutePath.TrimStart('/')
    } else {
        # In particular, a bare github.com/owner/repository string is a local Git path.
        throw 'The expected push destination must be an HTTPS or SSH GitHub URL.'
    }

    if ($path -notmatch '^(?<owner>[^/]+)/(?<repository>[^/]+?)(?:\.git)?/?$') {
        throw 'The expected push destination must name one GitHub owner and repository.'
    }
    return ('github.com/{0}/{1}' -f $Matches.owner, ($Matches.repository -replace '\.git$', '')).ToLowerInvariant()
}

function Get-ValidatedPushUrl {
    $urls = @(& git remote get-url --push --all origin 2>$null)
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -or $urls.Count -ne 1) {
        throw 'Origin must have exactly one configured push destination. Nothing was committed or pushed.'
    }
    $url = [string]$urls[0]
    if ((Normalize-GitHubRemote $url) -cne (Normalize-GitHubRemote $ExpectedPushDestination)) {
        throw 'The configured push destination does not match the expected GitHub repository. Nothing was committed or pushed.'
    }
    return $url
}

function Get-CurrentBranch {
    $lines = @(& git branch --show-current 2>$null)
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -or $lines.Count -ne 1) { throw 'Could not identify the current branch. Nothing was committed or pushed.' }
    $branch = [string]$lines[0]
    if ([string]::IsNullOrEmpty($branch) -or $branch -in @('main', 'master')) {
        throw 'Publish from a named feature branch, not main or master. Nothing was committed or pushed.'
    }
    & git check-ref-format "refs/heads/$branch" 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'The current branch name is not a valid Git branch. Nothing was committed or pushed.' }
    return $branch
}

function Get-Head {
    $value = & git rev-parse --verify HEAD 2>$null
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -or [string]::IsNullOrEmpty([string]$value)) { throw 'Could not identify HEAD.' }
    return [string]$value
}

function Assert-NoMergeState([string]$head) {
    & git rev-parse --verify --quiet MERGE_HEAD 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { throw 'A merge is in progress. Finish or abort it before publishing.' }
    if ($LASTEXITCODE -ne 1) { throw 'Could not verify merge state. Nothing was committed or pushed.' }

    $parents = & git rev-list --parents -n 1 $head 2>$null
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) { throw 'Could not inspect the current commit parents.' }
    if (([string]$parents -split '\s+').Count -gt 2) { throw 'HEAD is a merge commit. Publish from a single-parent commit.' }
}

function Assert-NoUnstagedOrUntrackedChanges {
    & git diff --quiet --exit-code 2>$null
    $diffExitCode = $LASTEXITCODE
    if ($diffExitCode -eq 1) { throw 'Unstaged changes exist. Stage or discard them, then review the staged tree.' }
    if ($diffExitCode -ne 0) { throw 'Could not inspect unstaged changes.' }

    $untracked = @(& git ls-files --others --exclude-standard 2>$null)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect untracked files.' }
    if ($untracked.Count -gt 0) { throw 'Untracked files exist. Stage or remove them before publishing.' }
}

Push-Location -LiteralPath $repo
try {
    $expectedDestination = Normalize-GitHubRemote $ExpectedPushDestination
    $branch = Get-CurrentBranch
    $pushUrlBeforeConfirmation = Get-ValidatedPushUrl
    $headBeforeConfirmation = Get-Head
    Assert-NoMergeState $headBeforeConfirmation
    Assert-NoUnstagedOrUntrackedChanges

    $stagedTree = & git write-tree 2>$null
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -or [string]::IsNullOrEmpty([string]$stagedTree)) { throw 'Could not record the staged tree.' }
    $stagedTree = [string]$stagedTree
    $staged = @(& git -c core.quotePath=false diff-tree --no-commit-id --no-renames --name-only -r $headBeforeConfirmation $stagedTree 2>$null)
    if ($LASTEXITCODE -ne 0) { throw 'Could not list files from the recorded staged tree.' }
    if ($staged.Count -eq 0) { throw 'There are no staged changes. This script will not stage files automatically.' }
    if (@($staged | Where-Object { $_ -match '(^|/)AGENTS\.md$' }).Count -gt 0) {
        throw 'AGENTS.md is staged. Unstage it before publishing.'
    }

    if ([string]::IsNullOrWhiteSpace($CommitMessage)) {
        $CommitMessage = "FilmCraft Timeline updates`n`nUpdate the staged Timeline changes."
    }
    Write-Host 'Staged files from the recorded tree:'
    $staged | ForEach-Object { Write-Host "  $_" }
    Write-Host ''
    Write-Host 'Commit message preview:'
    Write-Host $CommitMessage
    Write-Host ''
    $confirmation = Read-Host "Type YES to commit this tree and push to $expectedDestination"
    if ($confirmation -cne 'YES') {
        Write-Host 'Cancelled. Nothing was committed or pushed.'
        exit 0
    }

    if ((Get-Head) -cne $headBeforeConfirmation) { throw 'HEAD changed after confirmation. Nothing was pushed.' }
    if ((Get-CurrentBranch) -cne $branch) { throw 'The branch changed after confirmation. Nothing was pushed.' }
    Assert-NoMergeState $headBeforeConfirmation
    Assert-NoUnstagedOrUntrackedChanges
    $treeNow = & git write-tree 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]$treeNow -cne $stagedTree) { throw 'The staged tree changed after confirmation. Nothing was pushed.' }
    $pushUrlNow = Get-ValidatedPushUrl
    if ((Normalize-GitHubRemote $pushUrlNow) -cne $expectedDestination -or
        (Normalize-GitHubRemote $pushUrlBeforeConfirmation) -cne $expectedDestination) {
        throw 'The validated push destination changed after confirmation. Nothing was pushed.'
    }

    & git commit -m $CommitMessage
    if ($LASTEXITCODE -ne 0) { throw 'Commit failed. Nothing was pushed.' }

    $commitId = Get-Head
    $committedTree = & git rev-parse "$commitId^{tree}" 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]$committedTree -cne $stagedTree) {
        throw 'The new commit does not match the confirmed staged tree. Inspect the local commit before pushing.'
    }
    $parents = & git rev-list --parents -n 1 $commitId 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the new commit parents. The commit is local; nothing was pushed.' }
    $parentIds = ([string]$parents -split '\s+')
    if ($parentIds.Count -ne 2 -or $parentIds[1] -cne $headBeforeConfirmation) {
        throw 'The new commit does not have the confirmed single parent. The commit is local; nothing was pushed.'
    }
    if ((Get-CurrentBranch) -cne $branch) { throw 'The branch changed after commit. The commit is local; nothing was pushed.' }

    $pushUrlNow = Get-ValidatedPushUrl
    if ((Normalize-GitHubRemote $pushUrlNow) -cne $expectedDestination) {
        throw 'The validated push destination changed after commit. The commit is local; nothing was pushed.'
    }
    if ((Get-Head) -cne $commitId) { throw 'HEAD changed after commit. The commit is local; nothing was pushed.' }
    & git push --no-follow-tags --no-recurse-submodules $pushUrlNow "$($commitId):refs/heads/$branch" 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw 'Push failed. The remote may have received the commit; verify the branch before retrying.'
    }

    Write-Host "Published commit $($commitId.Substring(0, 12)) to the validated GitHub destination."
} finally {
    Pop-Location
}
