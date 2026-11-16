param(
    [Parameter(Mandatory)][int]$PullRequest,
    [Parameter(Mandatory)][string]$Commit
)

$ErrorActionPreference = 'Stop'
if ($PullRequest -le 0 -or $Commit -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Test releases require a positive PR number and exact source commit.'
}
$pr = Invoke-RestMethod -Uri "https://api.github.com/repos/microsoft/rayfin-local/pulls/$PullRequest" `
    -Headers @{ Accept = 'application/vnd.github+json'; 'X-GitHub-Api-Version' = '2022-11-28' }
if ($pr.state -cne 'open' -or
    $pr.head.repo.full_name -cne 'microsoft/rayfin-local' -or
    $pr.base.repo.full_name -cne 'microsoft/rayfin-local' -or
    $pr.base.ref -cne 'main' -or
    $pr.head.sha -cne $Commit -or
    'release-test' -cnotin @($pr.labels | ForEach-Object { $_.name })) {
    throw "PR #$PullRequest must be open, target main in microsoft/rayfin-local, have the release-test label, and still point to commit $Commit. Forks are not allowed."
}
Write-Host "PR #$PullRequest authorizes an alpha release of its exact current commit $Commit."
