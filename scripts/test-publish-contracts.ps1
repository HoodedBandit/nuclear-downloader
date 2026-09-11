$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$workflow = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot '.github/workflows/publish-release.yml')
$manualVerificationIndex = $workflow.IndexOf('& scripts/verify-manual-acceptance-evidence.ps1')
$qualificationRetentionIndex = $workflow.IndexOf('- name: Retain private qualification record')
$draftStepIndex = $workflow.IndexOf('- name: Create or recover and independently verify a draft release')
if (-not $workflow.Contains('manual_qualification:') -or
    -not $workflow.Contains('default: complete') -or
    -not $workflow.Contains('manual_acceptance_json:') -or
    $workflow.Contains('manual_acceptance_confirmation:') -or
    $workflow.Contains('COOKIE AND RUNTIME ACCEPTED') -or
    -not $workflow.Contains('-ExpectedSubmitter $env:EXPECTED_MANUAL_SUBMITTER') -or
    -not $workflow.Contains("'PUBLISH v0.7.1 WITH MANUAL CHECKS PENDING'") -or
    -not $workflow.Contains("'status=incomplete' >> `$env:GITHUB_OUTPUT") -or
    -not $workflow.Contains('pending-manual-qualification.json') -or
    -not $workflow.Contains('name: nuclear-downloader-0.7.1-qualification') -or
    $manualVerificationIndex -lt 0 -or
    $qualificationRetentionIndex -le $manualVerificationIndex -or
    $draftStepIndex -le $qualificationRetentionIndex) {
    throw 'The complete and pending qualification records must be retained before any draft release mutation.'
}
$candidateMatch = [regex]::Match($workflow, '(?ms)^      - name: Validate protected publish approval and candidate run\r?\n.*?^        run: \|\r?\n(?<code>.*?)(?=^      - name:)')
$qualificationMatch = [regex]::Match($workflow, '(?ms)^      - name: Verify downloaded bytes and source identity\r?\n.*?^        run: \|\r?\n(?<code>.*?)(?=^      - name:)')
$draftMatch = [regex]::Match($workflow, '(?ms)^      - name: Create or recover and independently verify a draft release\r?\n.*?^        run: \|\r?\n(?<code>.*?)(?=^      - name:)')
$publishMatch = [regex]::Match($workflow, '(?ms)^      - name: Publish the exact verified draft\r?\n.*?^        run: \|\r?\n(?<code>.*)\z')
if (-not $candidateMatch.Success -or -not $qualificationMatch.Success -or
    -not $draftMatch.Success -or -not $publishMatch.Success) {
    throw 'The production publish steps could not be located for executable contract tests.'
}
$candidateCode = [scriptblock]::Create([regex]::Replace($candidateMatch.Groups['code'].Value, '(?m)^          ', ''))
$qualificationText = [regex]::Replace($qualificationMatch.Groups['code'].Value, '(?m)^          ', '')
$qualificationText = $qualificationText.Replace("'`${{ inputs.candidate_run_id }}'", "'34452333097'")
$qualificationCode = [scriptblock]::Create($qualificationText)
$draftCode = [scriptblock]::Create([regex]::Replace($draftMatch.Groups['code'].Value, '(?m)^          ', ''))
$publishCode = [scriptblock]::Create([regex]::Replace($publishMatch.Groups['code'].Value, '(?m)^          ', ''))

$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-publish-tests-$([Guid]::NewGuid().ToString('N'))"
$savedEnvironment = @{}
foreach ($name in @(
    'GH_REPO',
    'EXPECTED_COMMIT_SHA',
    'EXPECTED_RELEASE_VERSION',
    'EXPECTED_MANUAL_SUBMITTER',
    'GITHUB_OUTPUT',
    'INPUT_CANDIDATE_RUN_ID',
    'INPUT_CONFIRMATION',
    'INPUT_MANUAL_ACCEPTANCE_JSON',
    'INPUT_MANUAL_QUALIFICATION',
    'INPUT_RELEASE_VERSION',
    'MANUAL_QUALIFICATION',
    'VERIFIED_RELEASE_ID'
)) {
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
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/actions/runs/$env:INPUT_CANDIDATE_RUN_ID") {
        return ConvertTo-Json -InputObject $script:candidateRun -Depth 10 -Compress
    }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/releases?per_page=100") {
        $pages = [object[]]::new(2)
        $pages[0] = @(@{ tag_name = 'v0.5.4'; draft = $false })
        $pages[1] = @($script:drafts)
        return ConvertTo-Json -InputObject $pages -Depth 10 -Compress
    }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/git/matching-refs/tags/v0.7.1") {
        return ConvertTo-Json -InputObject @($script:tagRefs) -Depth 10 -Compress
    }
    if ($args[0] -ceq 'api' -and $args[1] -ceq "repos/$env:GH_REPO/releases/123") {
        $release = @{} + $script:drafts[0]
        $release.name = if ($script:wrongTitle) { 'Wrong title' } else { 'Nuclear Downloader 0.7.1' }
        $release.body = if ($script:wrongBody) {
            'Wrong body'
        } else {
            [System.IO.File]::ReadAllText((Join-Path $fixtureRoot 'release-notes.md'))
        }
        return ConvertTo-Json -InputObject $release -Depth 10 -Compress
    }
    if ($args[0] -ceq 'release' -and $args[1] -ceq 'create') {
        $script:createCount++
        if ($args[2] -cne 'v0.7.1' -or $args -cnotcontains '--draft' -or
            $args[([array]::IndexOf($args, '--target') + 1)] -cne $env:EXPECTED_COMMIT_SHA) {
            throw 'The workflow attempted to create a release with the wrong identity or visibility.'
        }
        $notesFlagIndex = [array]::IndexOf($args, '--notes-file')
        if ($notesFlagIndex -lt 0) { throw 'Release notes must use a file argument.' }
        $notesPath = [string]$args[$notesFlagIndex + 1]
        if ($notesPath -cne (Join-Path $fixtureRoot 'release-notes.md') -or
            -not (Test-Path -LiteralPath $notesPath -PathType Leaf) -or
            -not ([IO.File]::ReadAllText($notesPath).Contains("`n### Highlights`n"))) {
            throw 'Release notes must preserve newlines in the owned working directory.'
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
        id = 123; tag_name = 'v0.7.1'; target_commitish = ('b' * 40)
        draft = $true; prerelease = $false
        assets = @($fixtureAssets | ForEach-Object {
            @{ name = $_.fileName; size = $_.size; digest = "sha256:$($_.sha256)"; state = 'uploaded' }
        })
    }
    $script:drafts = @($script:validDraft)
    $script:createCount = 0
    $script:apiFailure = $false
    $script:tagRefs = @()
    $script:hideCreatedDraft = $false
    $script:wrongTitle = $false
    $script:wrongBody = $false
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
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'acceptance') | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'scripts') | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureRoot 'candidate/release-candidate-inventory.json'),
        (ConvertTo-Json -InputObject @{ assets = $fixtureAssets } -Depth 10),
        [System.Text.UTF8Encoding]::new($false)
    )
    $env:GH_REPO = 'fixture/never-contacted'
    $env:EXPECTED_COMMIT_SHA = ('b' * 40)
    $env:EXPECTED_RELEASE_VERSION = '0.7.1'
    $env:EXPECTED_MANUAL_SUBMITTER = 'fixture-maintainer'
    $env:GITHUB_OUTPUT = Join-Path $fixtureRoot 'outputs.txt'
    Push-Location $fixtureRoot
    try {
        $script:candidateRun = @{
            name = 'Release Candidate'
            path = '.github/workflows/release-candidate.yml'
            event = 'workflow_dispatch'
            status = 'completed'
            conclusion = 'success'
            head_repository = @{ full_name = $env:GH_REPO }
            head_sha = $env:EXPECTED_COMMIT_SHA
        }
        $script:apiFailure = $false
        $script:requests = [System.Collections.Generic.List[string]]::new()
        function Invoke-CandidateValidationCase {
            param(
                [string] $Mode,
                [string] $Confirmation,
                [string] $ManualJson,
                [bool] $Reject
            )
            $env:INPUT_RELEASE_VERSION = '0.7.1'
            $env:INPUT_CANDIDATE_RUN_ID = '34452333097'
            $env:INPUT_MANUAL_QUALIFICATION = $Mode
            $env:INPUT_CONFIRMATION = $Confirmation
            $env:INPUT_MANUAL_ACCEPTANCE_JSON = $ManualJson
            [System.IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
            $errorText = $null
            try { & $candidateCode *> $null } catch { $errorText = $_.Exception.Message }
            if (($null -ne $errorText) -ne $Reject) {
                throw "Candidate validation returned the wrong result for mode '$Mode': $errorText"
            }
        }
        Invoke-CandidateValidationCase 'complete' 'PUBLISH v0.7.1' '{}' $false
        Invoke-CandidateValidationCase 'pending' 'PUBLISH v0.7.1 WITH MANUAL CHECKS PENDING' '' $false
        Invoke-CandidateValidationCase 'pending' 'PUBLISH v0.7.1' '' $true
        Invoke-CandidateValidationCase 'complete' 'PUBLISH v0.7.1 WITH MANUAL CHECKS PENDING' '{}' $true
        Invoke-CandidateValidationCase 'pending' 'PUBLISH v0.7.1 WITH MANUAL CHECKS PENDING' '{}' $true
        Invoke-CandidateValidationCase 'pending' 'PUBLISH v0.7.1 WITH MANUAL CHECKS PENDING' ' ' $true
        Invoke-CandidateValidationCase 'complete' 'PUBLISH v0.7.1' '' $true
        Invoke-CandidateValidationCase 'unexpected' 'PUBLISH v0.7.1' '' $true
        Write-Output 'Passed: qualification mode, confirmation, and pending JSON validation'

        $stub = @'
param(
    [string] $CandidateDirectory,
    [string] $EvidenceDirectory,
    [string] $EvidencePath,
    [string] $ExpectedVersion,
    [string] $ExpectedCommitSha,
    [string] $ExpectedCandidateRunId,
    [string] $ExpectedSubmitter
)
$global:qualificationCalls.Add([System.IO.Path]::GetFileName($MyInvocation.MyCommand.Path))
if ($global:qualificationFailure -ceq [System.IO.Path]::GetFileName($MyInvocation.MyCommand.Path)) {
    throw "Injected verifier failure: $global:qualificationFailure"
}
'@
        foreach ($name in @(
            'verify-release-candidate.ps1',
            'verify-acceptance-evidence.ps1',
            'verify-manual-acceptance-evidence.ps1'
        )) {
            [System.IO.File]::WriteAllText(
                (Join-Path $fixtureRoot "scripts/$name"),
                $stub,
                [System.Text.UTF8Encoding]::new($false)
            )
        }
        $requirements = @(
            'clean-windows11-installer',
            'clean-windows11-portable',
            'youtube-maintainer-fixture',
            'x-maintainer-fixture',
            'dedicated-account-cookie-login',
            'signed-app-update',
            'signed-runtime-update-rollback'
        )
        [System.IO.File]::WriteAllText(
            (Join-Path $fixtureRoot 'acceptance/windows-x64-acceptance.json'),
            (ConvertTo-Json -InputObject @{ incompleteRequirements = $requirements }),
            [System.Text.UTF8Encoding]::new($false)
        )
        function Invoke-QualificationCase {
            param(
                [string] $Mode,
                [string] $ManualJson,
                [bool] $Reject,
                [string] $FailAt = ''
            )
            $qualificationRoot = [System.IO.Path]::GetFullPath((Join-Path $fixtureRoot 'qualification'))
            $canonicalFixtureRoot = [System.IO.Path]::GetFullPath($fixtureRoot)
            if ([System.IO.Path]::GetDirectoryName($qualificationRoot) -cne $canonicalFixtureRoot -or
                [System.IO.Path]::GetFileName($qualificationRoot) -cne 'qualification') {
                throw 'Qualification fixture cleanup escaped its owned fixture root.'
            }
            if (Test-Path -LiteralPath $qualificationRoot) {
                Remove-Item -LiteralPath $qualificationRoot -Recurse -Force
            }
            [System.IO.File]::WriteAllText($env:GITHUB_OUTPUT, '')
            $global:qualificationCalls = [System.Collections.Generic.List[string]]::new()
            $global:qualificationFailure = $FailAt
            $env:INPUT_MANUAL_QUALIFICATION = $Mode
            $env:INPUT_MANUAL_ACCEPTANCE_JSON = $ManualJson
            $errorText = $null
            try { & $qualificationCode *> $null } catch { $errorText = $_.Exception.Message }
            if (($null -ne $errorText) -ne $Reject) {
                throw "Qualification case '$Mode' returned the wrong result: $errorText"
            }
            $minimumCalls = if ($FailAt -ceq 'verify-release-candidate.ps1') { 1 } else { 2 }
            if ($global:qualificationCalls.Count -lt $minimumCalls -or
                $global:qualificationCalls[0] -cne 'verify-release-candidate.ps1' -or
                ($minimumCalls -eq 2 -and
                    $global:qualificationCalls[1] -cne 'verify-acceptance-evidence.ps1')) {
                throw "Qualification case '$Mode' bypassed unconditional candidate verification."
            }
            if (-not $Reject) {
                $expectedStatus = if ($Mode -ceq 'complete') { 'status=complete' } else { 'status=incomplete' }
                if ([System.IO.File]::ReadAllText($env:GITHUB_OUTPUT).Trim() -cne $expectedStatus) {
                    throw "Qualification case '$Mode' emitted the wrong verified status."
                }
            } elseif ([System.IO.File]::ReadAllText($env:GITHUB_OUTPUT).Length -ne 0) {
                throw "Rejected qualification case '$Mode' emitted a verified status."
            }
        }
        Invoke-QualificationCase 'pending' '' $false
        $pendingJson = Get-Content -Raw -LiteralPath qualification/pending-manual-qualification.json
        $pending = $pendingJson | ConvertFrom-Json
        $inventoryHash = (Get-FileHash -LiteralPath candidate/release-candidate-inventory.json -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($pending.qualificationStatus -cne 'incomplete' -or
            $pending.sourceCommit -cne $env:EXPECTED_COMMIT_SHA -or
            $pending.candidateRunId -cne '34452333097' -or
            $pending.candidateInventorySha256 -cne $inventoryHash -or
            $pending.authorizedBy -cne $env:EXPECTED_MANUAL_SUBMITTER -or
            $pendingJson -cnotmatch '"recordedAt"\s*:\s*"20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z"' -or
            @($pending.incompleteRequirements).Count -ne 7 -or
            (Compare-Object @($pending.incompleteRequirements) $requirements)) {
            throw 'Pending qualification record was not bound to the verified automated evidence.'
        }
        Invoke-QualificationCase 'complete' '{}' $false
        if (-not (Test-Path qualification/windows-x64-manual-acceptance.json) -or
            $global:qualificationCalls[2] -cne 'verify-manual-acceptance-evidence.ps1' -or
            (Get-Content -Raw qualification/windows-x64-manual-acceptance.json) -cne '{}') {
            throw 'Complete qualification did not retain and verify the supplied manual record.'
        }
        Invoke-QualificationCase 'complete' '' $true
        Invoke-QualificationCase 'pending' '{}' $true
        Invoke-QualificationCase 'invalid' '' $true
        Invoke-QualificationCase 'pending' '' $true 'verify-release-candidate.ps1'
        if ($global:qualificationCalls.Count -ne 1 -or
            (Test-Path -LiteralPath qualification/pending-manual-qualification.json)) {
            throw 'Candidate verification failure reached a later verifier or emitted a qualification record.'
        }
        Invoke-QualificationCase 'pending' '' $true 'verify-acceptance-evidence.ps1'
        if ($global:qualificationCalls.Count -ne 2 -or
            (Test-Path -LiteralPath qualification/pending-manual-qualification.json)) {
            throw 'Automated acceptance failure reached manual qualification or emitted a status record.'
        }
        Invoke-QualificationCase 'complete' '{}' $true 'verify-manual-acceptance-evidence.ps1'
        if ($global:qualificationCalls.Count -ne 3) {
            throw 'Manual verification failure did not stop at the manual verifier.'
        }
        $global:qualificationFailure = ''
        Write-Output 'Passed: complete and pending qualification records are fail-closed and candidate-bound'

        $env:MANUAL_QUALIFICATION = 'complete'
        Invoke-PublishCase 'new draft verified by ID, not unpublished tag' { $script:drafts = @() } -Reject $false -ExpectedCreates 1
        Invoke-PublishCase 'matching existing draft recovered without uploading' {} -Reject $false
        Invoke-PublishCase 'new draft uses matching lightweight candidate tag' {
            $script:drafts = @()
            $script:tagRefs = @(@{
                ref = 'refs/tags/v0.7.1'
                object = @{ type = 'commit'; sha = $env:EXPECTED_COMMIT_SHA }
            })
        } -Reject $false -ExpectedCreates 1
        Invoke-PublishCase 'wrong draft title rejected' { $script:wrongTitle = $true }
        Invoke-PublishCase 'wrong prepared release notes rejected' { $script:wrongBody = $true }
        $env:MANUAL_QUALIFICATION = 'incomplete'
        Invoke-PublishCase 'pending qualification produces an exact verified draft' {} -Reject $false
        $pendingNotes = [System.IO.File]::ReadAllText((Join-Path $fixtureRoot 'release-notes.md'))
        if (-not $pendingNotes.Contains('Manual qualification remains pending:') -or
            $pendingNotes.Contains('the recorded manual qualification checks passed')) {
            throw 'Pending qualification release notes falsely claimed completed manual checks.'
        }
        Invoke-PublishCase 'unverified qualification status rejected' { $env:MANUAL_QUALIFICATION = 'pending' }
        $env:MANUAL_QUALIFICATION = 'complete'
        Invoke-PublishCase 'already published release remains immutable' { $script:validDraft.draft = $false }
        Invoke-PublishCase 'ambiguous drafts rejected' { $script:drafts = @($script:validDraft, $script:validDraft) }
        Invoke-PublishCase 'matching lightweight tag and existing draft accepted without asset upload' {
            $script:tagRefs = @(@{
                ref = 'refs/tags/v0.7.1'
                object = @{ type = 'commit'; sha = $env:EXPECTED_COMMIT_SHA }
            })
        } -Reject $false
        Invoke-PublishCase 'tag at wrong commit rejected' {
            $script:tagRefs = @(@{
                ref = 'refs/tags/v0.7.1'
                object = @{ type = 'commit'; sha = ('c' * 40) }
            })
        }
        Invoke-PublishCase 'annotated tag object rejected' {
            $script:tagRefs = @(@{
                ref = 'refs/tags/v0.7.1'
                object = @{ type = 'tag'; sha = $env:EXPECTED_COMMIT_SHA }
            })
        }
        Invoke-PublishCase 'duplicate exact tag refs rejected' {
            $tag = @{
                ref = 'refs/tags/v0.7.1'
                object = @{ type = 'commit'; sha = $env:EXPECTED_COMMIT_SHA }
            }
            $script:tagRefs = @($tag, $tag)
        }
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
        foreach ($badId in @('', 'v0.7.1', '../123')) {
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
