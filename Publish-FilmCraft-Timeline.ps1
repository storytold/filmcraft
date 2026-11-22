[CmdletBinding()]
param(
    [string] $CommitTitle = '',

    [string] $CommitBody = '',

    [string] $CommitMessage,

    [string] $Branch = '',

    [string] $ExpectedPushDestination = 'https://github.com/TheMiningPickaxe/filmcraft'
)

$ErrorActionPreference = 'Stop'

function Invoke-GitText {
    param([Parameter(Mandatory = $true)][string[]] $Arguments)
    $output = & git -C $script:RepositoryRoot @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "git $($Arguments -join ' ') failed: $($output -join [Environment]::NewLine)"
    }
    return ($output -join [Environment]::NewLine)
}

function Get-GitHubRepository {
    param([Parameter(Mandatory = $true)][string] $Remote)

    $path = $null
    if ($Remote -match '^https://github\.com/([^/]+)/([^/]+)/?$') {
        $path = "$($Matches[1])/$($Matches[2])"
    } elseif ($Remote -match '^git@github\.com:([^/]+)/([^/]+)/?$') {
        $path = "$($Matches[1])/$($Matches[2])"
    } elseif ($Remote -match '^ssh://git@github\.com/([^/]+)/([^/]+)/?$') {
        $path = "$($Matches[1])/$($Matches[2])"
    } else {
        throw "Not a GitHub HTTPS or SSH remote URL: $Remote"
    }

    $path = $path -replace '\.git$', ''
    if ($path -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
        throw "Invalid GitHub owner/repository remote: $Remote"
    }
    return $path.ToLowerInvariant()
}

function Assert-NoMergeInProgress {
    $mergeHead = & git -C $script:RepositoryRoot rev-parse --quiet --verify MERGE_HEAD 2>$null
    if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrEmpty(($mergeHead -join ''))) {
        throw 'An unfinished merge is in progress. Complete or abort it before publishing.'
    }
    $conflicts = Invoke-GitText @('diff', '--name-only', '--diff-filter=U')
    if (-not [string]::IsNullOrWhiteSpace($conflicts)) {
        throw "Unresolved merge conflicts remain:`n$conflicts"
    }
}

$RepositoryRoot = (& git -C $PSScriptRoot rev-parse --show-toplevel 2>$null | Select-Object -First 1)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    throw 'Could not locate the Git checkout containing this script.'
}
$script:RepositoryRoot = [System.IO.Path]::GetFullPath($RepositoryRoot)

$currentBranch = (Invoke-GitText @('branch', '--show-current'))
if ([string]::IsNullOrEmpty($Branch)) {
    $Branch = $currentBranch
}
if ([string]::IsNullOrEmpty($Branch)) {
    throw 'Publishing requires a named branch; detached HEAD is not allowed.'
}
if ($Branch -cne $currentBranch) {
    throw "The requested branch '$Branch' is not the checked-out branch '$currentBranch'."
}
if ($Branch -in @('main', 'master')) {
    throw "Refusing to publish protected branch '$Branch'."
}
$null = Invoke-GitText @('check-ref-format', '--branch', $Branch)
Assert-NoMergeInProgress

$expectedRepository = Get-GitHubRepository $ExpectedPushDestination
$pushUrlsText = Invoke-GitText @('remote', 'get-url', '--push', '--all', 'origin')
$pushUrls = @($pushUrlsText -split '\r?\n' | Where-Object { -not [string]::IsNullOrEmpty($_) })
if ($pushUrls.Count -eq 0) {
    throw 'The origin remote has no push URL.'
}
foreach ($pushUrl in $pushUrls) {
    $repository = Get-GitHubRepository $pushUrl
    if ($repository -cne $expectedRepository) {
        throw "origin push URL '$pushUrl' does not match expected destination '$ExpectedPushDestination'."
    }
}

$stagedText = & git -C $RepositoryRoot diff --cached --name-only --no-renames -z -- 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "Could not inspect the staged index: $($stagedText -join [Environment]::NewLine)"
}
$stagedPaths = @((($stagedText -join '') -split "`0") | Where-Object { $_.Length -gt 0 })
if ($stagedPaths.Count -eq 0) {
    throw 'There are no staged files to publish.'
}
foreach ($path in $stagedPaths) {
    $leaf = [System.IO.Path]::GetFileName($path)
    if ($leaf.Equals('AGENTS.md', [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to publish staged contributor instructions: $path"
    }
}

if (-not $PSBoundParameters.ContainsKey('CommitMessage') -and [string]::IsNullOrWhiteSpace($CommitTitle)) {
    throw 'Commit title must not be empty.'
}
if ($PSBoundParameters.ContainsKey('CommitMessage')) {
    if ([string]::IsNullOrWhiteSpace($CommitMessage)) {
        throw 'A custom commit message must not be empty.'
    }
    $message = $CommitMessage
} elseif ([string]::IsNullOrEmpty($CommitBody)) {
    $message = $CommitTitle
} else {
    $message = "$CommitTitle`n`n$CommitBody"
}

$headBefore = Invoke-GitText @('rev-parse', 'HEAD')
$treeBefore = Invoke-GitText @('write-tree')
Write-Host "Repository: $RepositoryRoot"
Write-Host "Branch: $Branch"
Write-Host "Push destination: $ExpectedPushDestination"
Write-Host 'Staged paths:'
$stagedPaths | ForEach-Object { Write-Host "  $_" }
Write-Host 'Commit message:'
Write-Host $message
Write-Host 'Run the required quality gates before publishing.'
$confirmation = Read-Host 'Type YES to commit and push this staged snapshot'
if ($confirmation -cne 'YES') {
    throw 'Publishing cancelled; no commit or push was made.'
}

Assert-NoMergeInProgress
$branchNow = Invoke-GitText @('branch', '--show-current')
$headNow = Invoke-GitText @('rev-parse', 'HEAD')
$treeNow = Invoke-GitText @('write-tree')
if ($branchNow -cne $Branch -or $headNow -cne $headBefore -or $treeNow -cne $treeBefore) {
    throw 'The branch, HEAD, or staged snapshot changed while confirmation was pending; review it again.'
}

if ($PSBoundParameters.ContainsKey('CommitMessage')) {
    $commitOutput = & git -C $RepositoryRoot commit -m $CommitMessage 2>&1
} else {
    $commitArgs = @('commit', '-m', $CommitTitle)
    if (-not [string]::IsNullOrEmpty($CommitBody)) {
        $commitArgs += @('-m', $CommitBody)
    }
    $commitOutput = & git -C $RepositoryRoot @commitArgs 2>&1
}
if ($LASTEXITCODE -ne 0) {
    throw "Commit failed before push: $($commitOutput -join [Environment]::NewLine)"
}
Write-Host ($commitOutput -join [Environment]::NewLine)

$pushOutput = & git -C $RepositoryRoot push --no-follow-tags $ExpectedPushDestination "refs/heads/${Branch}:refs/heads/${Branch}" 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "Push failed after the commit was created locally. The remote may have accepted it despite the error; verify the destination before retrying. $($pushOutput -join [Environment]::NewLine)"
}
Write-Host ($pushOutput -join [Environment]::NewLine)
Write-Host "Published $Branch to $ExpectedPushDestination."
