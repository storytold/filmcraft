param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CommitTitle,

    [string]$CommitBody = ''
)

$ErrorActionPreference = 'Stop'
$expectedRemote = 'https://github.com/TheMiningPickaxe/filmcraft'
if ([string]::IsNullOrWhiteSpace($CommitTitle)) {
    throw 'A commit title is required.'
}

function ConvertTo-CanonicalRepositoryUrl([string]$Url) {
    $value = $Url.Trim().TrimEnd('/')
    $value = $value -replace '^git@github\.com:', 'https://github.com/'
    $value = $value -replace '^ssh://git@github\.com/', 'https://github.com/'
    $value = $value -replace '\.git$', ''
    return $value
}

$repo = (& git -C $PSScriptRoot rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($repo)) {
    throw 'The script must be inside a FilmCraft Git checkout.'
}

$pushUrls = @(& git -C $repo remote get-url --push --all origin)
if ($LASTEXITCODE -ne 0 -or $pushUrls.Count -eq 0) {
    throw 'Could not read the origin push destination.'
}
foreach ($url in $pushUrls) {
    if ((ConvertTo-CanonicalRepositoryUrl $url) -ne $expectedRemote) {
        throw "The origin push destination '$url' is not the expected FilmCraft fork. Nothing was committed or pushed."
    }
}

$branch = (& git -C $repo branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($branch) -or $branch -in @('main', 'master')) {
    throw "Expected a feature branch; current branch is '$branch'. Nothing was committed or pushed."
}

$staged = @(& git -C $repo -c core.quotePath=false diff --cached --name-only --no-renames)
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect staged files.' }
if ($staged.Count -eq 0) {
    throw 'There are no staged changes. Stage the Timeline files first; this script will not stage files automatically.'
}
$stagedInstructions = @($staged | Where-Object { $_ -match '(^|/)AGENTS\.md$' })
if ($stagedInstructions.Count -gt 0) {
    throw 'AGENTS.md is staged. Unstage it before publishing; this script will not commit it.'
}
$headBeforeConfirmation = (& git -C $repo rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($headBeforeConfirmation)) {
    throw 'Could not record the current commit before confirmation.'
}
$stagedTreeBeforeConfirmation = (& git -C $repo write-tree).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($stagedTreeBeforeConfirmation)) {
    throw 'Could not record the staged tree before confirmation.'
}

Write-Host 'Staged files to be committed:'
$staged | ForEach-Object { Write-Host "  $_" }
Write-Host ''
Write-Host "Commit title: $CommitTitle"
if (-not [string]::IsNullOrWhiteSpace($CommitBody)) {
    Write-Host ''
    Write-Host $CommitBody
}
Write-Host ''
$confirmation = Read-Host "Type YES to commit these files and push branch '$branch' to your GitHub fork"
if ($confirmation -cne 'YES') {
    Write-Host 'Cancelled. Nothing was committed or pushed.'
    exit 0
}

$currentBranch = (& git -C $repo branch --show-current).Trim()
$currentHead = (& git -C $repo rev-parse HEAD).Trim()
$currentStagedTree = (& git -C $repo write-tree).Trim()
if ($LASTEXITCODE -ne 0 -or $currentBranch -cne $branch -or $currentHead -cne $headBeforeConfirmation -or $currentStagedTree -cne $stagedTreeBeforeConfirmation) {
    throw 'The branch, HEAD, or staged changes changed after confirmation. Nothing was committed or pushed; review and confirm again.'
}
if ([string]::IsNullOrWhiteSpace($CommitBody)) {
    & git -C $repo commit -m $CommitTitle
} else {
    & git -C $repo commit -m $CommitTitle -m $CommitBody
}
if ($LASTEXITCODE -ne 0) { throw 'Commit failed; nothing was pushed.' }

& git -C $repo push --no-follow-tags origin $branch
if ($LASTEXITCODE -ne 0) { throw 'Commit succeeded, but pushing failed. The commit is still local.' }

Write-Host ''
Write-Host "Published branch '$branch' to $expectedRemote."
