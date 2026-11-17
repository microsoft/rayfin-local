$ErrorActionPreference = 'Stop'
$commit = 'a' * 40
$original = @{
    state = 'open'
    head = @{ sha = $commit; repo = @{ full_name = 'microsoft/rayfin-local' } }
    base = @{ ref = 'main'; repo = @{ full_name = 'microsoft/rayfin-local' } }
    labels = @(@{ name = 'release-test' })
} | ConvertTo-Json -Depth 5

function Invoke-RestMethod {
    param($Uri, $Headers)
    if ($Uri -cne 'https://api.github.com/repos/microsoft/rayfin-local/pulls/11') {
        throw "Unexpected PR API URL: $Uri"
    }
    return $global:ReleaseTestPrResponse
}

$global:ReleaseTestPrResponse = $original | ConvertFrom-Json
& "$PSScriptRoot/Test-ReleasePullRequest.ps1" -PullRequest 11 -Commit $commit

foreach ($scenario in @('unlabelled', 'wrong-case-label', 'closed', 'fork', 'changed-head', 'wrong-base')) {
    $global:ReleaseTestPrResponse = $original | ConvertFrom-Json
    switch ($scenario) {
        'unlabelled' { $global:ReleaseTestPrResponse.labels = @() }
        'wrong-case-label' { $global:ReleaseTestPrResponse.labels[0].name = 'Release-Test' }
        'closed' { $global:ReleaseTestPrResponse.state = 'closed' }
        'fork' { $global:ReleaseTestPrResponse.head.repo.full_name = 'someone/rayfin-local' }
        'changed-head' { $global:ReleaseTestPrResponse.head.sha = 'b' * 40 }
        'wrong-base' { $global:ReleaseTestPrResponse.base.ref = 'other' }
    }
    $rejected = $false
    try {
        & "$PSScriptRoot/Test-ReleasePullRequest.ps1" -PullRequest 11 -Commit $commit
    }
    catch {
        if ($_.Exception.Message -notlike 'PR #11 must be open*') { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw "Accepted an unauthorized release request: $scenario" }
}
Remove-Variable -Name ReleaseTestPrResponse -Scope Global
Write-Host 'PR release opt-in checks passed.'
