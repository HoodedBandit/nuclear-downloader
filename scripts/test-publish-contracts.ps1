$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$workflow = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot '.github/workflows/publish-release.yml')
$draftMatch = [regex]::Match($workflow, '(?ms)^      - name: Create or recover and independently verify a draft release\r?\n.*?^        run: \|\r?\n(?<code>.*?)(?=^      - name:)')
$publishMatch = [regex]::Match($workflow, '(?ms)^      - name: Publish the exact verified draft\r?\n.*?^        run: \|\r?\n(?<code>.*)\z')
if (-not $draftMatch.Success -or -not $publishMatch.Success) {
    throw 'The production publish steps could not be located for executable contract tests.'
}
$draftCode = [scriptblock]::Create([regex]::Replace($draftMatch.Groups['code'].Value, '(?m)^          ', ''))
$publishCode = [scriptblock]::Create([regex]::Replace($publishMatch.Groups['code'].Value, '(?m)^          ', ''))

$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-publish-tests-$([Guid]::NewGuid().ToString('N'))"
$savedEnvironment = @{}
foreach ($name in @('GH_REPO', 'EXPECTED_COMMIT_SHA', 'GITHUB_OUTPUT', 'VERIFIED_RELEASE_ID')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$fixtureAssets = @(1..10 | ForEach-Object {
    @{ fileName = "fixture-$_.bin"; size = 123; sha256 = ('a' * 64) }
})

# Execute the actual workflow scripts with a closed CLI fake. No network, upload,
# tag, release, or artifact mutation is possible from this fixture.
function gh {
    $global:LASTEXITCODE = 0
    $script:requests.Add(($args -join ' '))
    if ($script:apiFailure) { throw 'Fixture API failure.' }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/releases?per_page=100") {
        $pages = [object[]]::new(2)
        $pages[0] = @(@{ tag_name = 'v0.5.4'; draft = $false })
        $pages[1] = @($script:drafts)
        return ConvertTo-Json -InputObject $pages -Depth 10 -Compress
    }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/git/matching-refs/tags/v0.6.0") {
        if ($script:tagExists) { return '[{"ref":"refs/tags/v0.6.0"}]' }
        return '[]'
    }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/releases/123") {
        return ConvertTo-Json -InputObject $script:drafts[0] -Depth 10 -Compress
    }
    if ($args[0] -ceq 'release' -and $args[1] -ceq 'create') {
        $script:createCount++
        if ($args[2] -cne 'v0.6.0' -or $args -cnotcontains '--draft' -or
            $args[([array]::IndexOf($args, '--target') + 1)] -cne $env:EXPECTED_COMMIT_SHA) {
            throw 'The workflow attempted to create a release with the wrong identity or visibility.'
        }
        if (-not $script:hideCreatedDraft) { $script:drafts = @($script:validDraft) }
        return 'https://example.invalid/releases/tag/untagged-fixture'
    }
    if (($args -join ' ') -ceq "api --method PATCH repos/$env:GH_REPO/releases/123 -F draft=false -f make_latest=true --silent") {
        $script:publishCount++
        return
    }
    throw "Unexpected CLI request in closed publish fixture: $($args -join ' ')"
}

function Invoke-PublishCase {
    param([string] $Name, [scriptblock] $Mutate, [bool] $Reject = $true, [int] $ExpectedCreates = 0)
    $script:validDraft = @{
        id = 123; tag_name = 'v0.6.0'; target_commitish = ('b' * 40)
        draft = $true; prerelease = $false
        assets = @($fixtureAssets | ForEach-Object {
            @{ name = $_.fileName; size = $_.size; digest = "sha256:$($_.sha256)"; state = 'uploaded' }
        })
    }
    $script:drafts = @($script:validDraft)
    $script:createCount = 0
    $script:apiFailure = $false
    $script:tagExists = $false
    $script:hideCreatedDraft = $false
    $script:requests = [System.Collections.Generic.List[string]]::new()
    [System.IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
    & $Mutate
    $errorText = $null
    try { & $draftCode *> $null } catch { $errorText = $_.Exception.Message }
    if (($null -ne $errorText) -ne $Reject) {
        throw "Publish case '$Name' returned the wrong result: $errorText"
    }
    if ($script:createCount -ne $ExpectedCreates) {
        throw "Publish case '$Name' unexpectedly created or replaced a draft."
    }
    $outputs = [System.IO.File]::ReadAllText($env:GITHUB_OUTPUT)
    if ($Reject -and $outputs.Length -ne 0) {
        throw "Rejected publish case '$Name' emitted a publishable release ID."
    }
    if (-not $Reject -and $outputs.Trim() -cne 'release_id=123') {
        throw "Publish case '$Name' did not emit the exact verified release ID."
    }
    Write-Output "Passed: $Name"
}

try {
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'candidate') | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureRoot 'candidate/release-candidate-inventory.json'),
        (ConvertTo-Json -InputObject @{ assets = $fixtureAssets } -Depth 10),
        [System.Text.UTF8Encoding]::new($false)
    )
    $env:GH_REPO = 'fixture/never-contacted'
    $env:EXPECTED_COMMIT_SHA = ('b' * 40)
    $env:GITHUB_OUTPUT = Join-Path $fixtureRoot 'outputs.txt'
    Push-Location $fixtureRoot
    try {
        Invoke-PublishCase 'new draft verified by ID, not unpublished tag' { $script:drafts = @() } -Reject $false -ExpectedCreates 1
        Invoke-PublishCase 'matching existing draft recovered without uploading' {} -Reject $false
        Invoke-PublishCase 'already published release remains immutable' { $script:validDraft.draft = $false }
        Invoke-PublishCase 'ambiguous drafts rejected' { $script:drafts = @($script:validDraft, $script:validDraft) }
        Invoke-PublishCase 'existing tag rejected' { $script:tagExists = $true }
        Invoke-PublishCase 'API failure rejected before mutation' { $script:apiFailure = $true }
        Invoke-PublishCase 'wrong source commit rejected' { $script:validDraft.target_commitish = ('c' * 40) }
        Invoke-PublishCase 'prerelease rejected' { $script:validDraft.prerelease = $true }
        Invoke-PublishCase 'malformed release ID rejected' { $script:validDraft.id = '../123' }
        Invoke-PublishCase 'missing asset rejected' { $script:validDraft.assets = @($script:validDraft.assets[0..8]) }
        Invoke-PublishCase 'extra asset rejected' { $script:validDraft.assets += $script:validDraft.assets[0] }
        Invoke-PublishCase 'duplicate filename rejected' { $script:validDraft.assets[1].name = $script:validDraft.assets[0].name }
        Invoke-PublishCase 'wrong filename case rejected' { $script:validDraft.assets[0].name = 'FIXTURE-1.bin' }
        Invoke-PublishCase 'wrong size rejected' { $script:validDraft.assets[0].size = 124 }
        Invoke-PublishCase 'unfinished upload rejected' { $script:validDraft.assets[0].state = 'starter' }
        Invoke-PublishCase 'wrong digest rejected' { $script:validDraft.assets[0].digest = ('c' * 64) }
        Invoke-PublishCase 'missing digest rejected' { $script:validDraft.assets[0].Remove('digest') }
        Invoke-PublishCase 'new draft lookup failure never emits publish ID' {
            $script:drafts = @(); $script:hideCreatedDraft = $true
        } -ExpectedCreates 1

        $script:publishCount = 0
        $script:apiFailure = $false
        $env:VERIFIED_RELEASE_ID = '123'
        & $publishCode
        if ($script:publishCount -ne 1) { throw 'Publishing did not use the exact verified numeric release ID.' }
        foreach ($badId in @('', 'v0.6.0', '../123')) {
            $env:VERIFIED_RELEASE_ID = $badId
            $rejected = $false
            try { & $publishCode } catch { $rejected = $true }
            if (-not $rejected -or $script:publishCount -ne 1) { throw 'An unverified release ID reached the publish API.' }
        }
        Write-Output 'Passed: publish uses the verified release ID and rejects invalid IDs'
    } finally {
        Pop-Location
    }
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name])
    }
    $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $canonicalFixture = [System.IO.Path]::GetFullPath($fixtureRoot)
    $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalFixture)
    if ($relative -match '^nuclear-publish-tests-[0-9a-f]{32}$' -and
        (Test-Path -LiteralPath $canonicalFixture)) {
        Remove-Item -LiteralPath $canonicalFixture -Recurse -Force
    }
}

Write-Output 'Production draft creation, exact-byte recovery, and publish contracts passed.'
