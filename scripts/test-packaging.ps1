[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-packaging-tests-$([Guid]::NewGuid().ToString('N'))"
$testMarker = Join-Path $testRoot '.nuclear-packaging-test-owned'

function Assert-True {
    param(
        [Parameter(Mandatory)] [bool] $Condition,
        [Parameter(Mandatory)] [string] $Message
    )
    if (-not $Condition) {
        throw $Message
    }
}

function Write-Utf8Json {
    param(
        [Parameter(Mandatory)] [object] $Value,
        [Parameter(Mandatory)] [string] $Path,
        [int] $Depth = 8
    )
    [System.IO.File]::WriteAllText(
        $Path,
        ($Value | ConvertTo-Json -Depth $Depth -Compress),
        $utf8NoBom
    )
}

function Get-LowerSha256 {
    param([Parameter(Mandatory)] [string] $Path)
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    [System.IO.File]::WriteAllText($testMarker, 'owned', $utf8NoBom)

    $scriptPaths = @(
        'scripts\build-release-candidate.ps1',
        'scripts\verify-release-candidate.ps1',
        'scripts\test-packaging.ps1',
        'scripts\package-runtime.ps1',
        'scripts\fetch-sidecars.ps1'
    )
    foreach ($relativePath in $scriptPaths) {
        $path = Join-Path $repositoryRoot $relativePath
        Assert-True -Condition (Test-Path -LiteralPath $path -PathType Leaf) -Message "Missing packaging script: $relativePath"
        $tokens = $null
        $parseErrors = $null
        [System.Management.Automation.Language.Parser]::ParseFile(
            $path,
            [ref]$tokens,
            [ref]$parseErrors
        ) | Out-Null
        Assert-True -Condition ($parseErrors.Count -eq 0) -Message "PowerShell parser errors in ${relativePath}: $($parseErrors -join '; ')"
    }

    $candidateWorkflowPath = Join-Path $repositoryRoot '.github\workflows\release-candidate.yml'
    $publishWorkflowPath = Join-Path $repositoryRoot '.github\workflows\publish-release.yml'
    $candidateWorkflow = Get-Content -Raw -LiteralPath $candidateWorkflowPath
    $publishWorkflow = Get-Content -Raw -LiteralPath $publishWorkflowPath
    foreach ($workflow in @($candidateWorkflow, $publishWorkflow)) {
        Assert-True -Condition ($workflow -match '(?m)^\s*workflow_dispatch:') -Message 'Release workflows must be manual workflow_dispatch jobs.'
        Assert-True -Condition ($workflow -notmatch 'uses:\s+[^\s]+@v\d') -Message 'Release workflows must not use mutable action version tags.'
        foreach ($sha in @('11d5960a326750d5838078e36cf38b85af677262')) {
            Assert-True -Condition ($workflow.Contains($sha)) -Message "Workflow is missing required pinned action SHA $sha."
        }
    }
    foreach ($sha in @(
        '49933ea5288caeca8642d1e84afbd3f7d6820020',
        '6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772'
    )) {
        Assert-True -Condition ($candidateWorkflow.Contains($sha)) -Message "Candidate workflow is missing required pinned action SHA $sha."
    }
    Assert-True -Condition ($candidateWorkflow.Contains('ea165f8d65b6e75b540449e92b4886f43607fa02')) -Message 'Candidate workflow must pin upload-artifact.'
    Assert-True -Condition ($candidateWorkflow -notmatch '(?m)^\s*gh\s+release\s') -Message 'Candidate workflow must never create or upload a GitHub Release.'
    Assert-True -Condition ($publishWorkflow.Contains('d3f86a106a0bac45b974a628896c90dbdf5c8093')) -Message 'Publish workflow must pin download-artifact.'
    Assert-True -Condition ($publishWorkflow.Contains('candidate_run_id')) -Message 'Publish workflow must download a prior candidate by run ID.'
    Assert-True -Condition ($publishWorkflow.Contains('PUBLISH v0.6.0')) -Message 'Publish workflow must require the exact maintainer confirmation.'
    Assert-True -Condition ($publishWorkflow.Contains('gh release create')) -Message 'Publish workflow must publish through gh without rebuilding.'
    Assert-True -Condition ($publishWorkflow -notmatch 'tauri\s+build|build-release-candidate') -Message 'Publish workflow must not rebuild candidate bytes.'
    foreach ($publicVariable in @(
        'vars.NUCLEAR_UPDATE_KEY_ID',
        'vars.NUCLEAR_UPDATE_PUBLIC_KEY',
        'vars.NUCLEAR_UPDATE_NEXT_KEY_ID',
        'vars.NUCLEAR_UPDATE_NEXT_PUBLIC_KEY'
    )) {
        Assert-True -Condition ($candidateWorkflow.Contains($publicVariable)) -Message "Candidate workflow is missing public trust variable $publicVariable."
        Assert-True -Condition ($publishWorkflow.Contains($publicVariable)) -Message "Publish workflow is missing public trust variable $publicVariable."
    }
    Assert-True -Condition ($candidateWorkflow.Contains('secrets.TAURI_SIGNING_PRIVATE_KEY')) -Message 'Candidate workflow must read the private signing key from a protected secret.'
    Assert-True -Condition ($candidateWorkflow.Contains('secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD')) -Message 'Candidate workflow must read the signing password from a protected secret.'
    Assert-True -Condition ($candidateWorkflow -notmatch 'secrets\.NUCLEAR_UPDATE_(?:NEXT_)?PUBLIC_KEY') -Message 'Public trust anchors belong in GitHub variables, not secret slots.'
    Assert-True -Condition ($publishWorkflow.Contains('cargo fetch --manifest-path nuclear-app/src-tauri/Cargo.toml --locked')) -Message 'Publish verification must fetch only Cargo.lock-pinned verifier source.'

    $buildScript = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'scripts\build-release-candidate.ps1')
    Assert-True -Condition ($buildScript.Contains('npm.cmd exec tauri signer sign')) -Message 'Candidate builder must sign manifests with the Tauri signer.'
    Assert-True -Condition ($buildScript.Contains('TAURI_SIGNING_PRIVATE_KEY_PASSWORD')) -Message 'Candidate builder must consume the protected signing password environment variable.'
    Assert-True -Condition ($buildScript.Contains('NUCLEAR_UPDATE_PUBLIC_KEY')) -Message 'Candidate builder must preflight the build.rs public-key contract.'
    $verifyScriptText = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'scripts\verify-release-candidate.ps1')
    Assert-True -Condition ($verifyScriptText.Contains('minisign-verify')) -Message 'Candidate verification must use the locked Minisign verifier.'
    Assert-True -Condition ($verifyScriptText.Contains('ConvertFrom-TauriMinisignWrapper')) -Message 'Candidate verification must decode Tauri CLI signature wrappers.'
    Assert-True -Condition ($verifyScriptText.Contains('22f9645cb765ea72b8111f36c522475d2daa0d22c957a9826437e97534bc4e9e')) -Message 'Candidate verification must pin the reviewed minisign-verify crate checksum.'
    Assert-True -Condition ($verifyScriptText.Contains('--locked')) -Message 'Candidate verification must resolve dependencies through Cargo.lock.'
    Assert-True -Condition ($verifyScriptText.Contains('--offline')) -Message 'Candidate verification must not resolve new dependencies after the locked fetch.'

    $fixtureRoot = Join-Path $testRoot 'candidate'
    New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
    $version = '0.6.0'
    $installerName = 'Nuclear.Downloader_0.6.0_x64-setup.exe'
    $portableName = 'Nuclear.Downloader_0.6.0_x64-portable.zip'
    $manifestName = 'nuclear-downloader-v0.6.0-update.json'
    $legacyName = 'nuclear-downloader-v0.6.0-sha256.txt'
    $runtimeVersion = '2026.07.04'
    $runtimeDescriptorName = 'nuclear-downloader-runtime-windows-x64.json'
    $runtimeArchiveName = "nuclear-downloader-runtime-$runtimeVersion-windows-x64.zip"
    $keyId = 'test-key-1'
    $testPrivateKeyPath = Join-Path $testRoot 'fixture-signing.key'
    $testPublicKeyPath = "$testPrivateKeyPath.pub"
    $testKeyPassword = "fixture-$([Guid]::NewGuid().ToString('N'))"
    Push-Location (Join-Path $repositoryRoot 'nuclear-app')
    try {
        & npm.cmd exec tauri signer generate -- `
            --ci `
            --password $testKeyPassword `
            --write-keys $testPrivateKeyPath *> $null
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not generate the disposable packaging-test signing key.'
        }
    } finally {
        Pop-Location
    }
    Assert-True -Condition (Test-Path -LiteralPath $testPrivateKeyPath -PathType Leaf) -Message 'Disposable test private key was not created.'
    Assert-True -Condition (Test-Path -LiteralPath $testPublicKeyPath -PathType Leaf) -Message 'Disposable test public key was not created.'
    $testPublicKey = Get-Content -Raw -LiteralPath $testPublicKeyPath

    [System.IO.File]::WriteAllBytes((Join-Path $fixtureRoot $installerName), [byte[]](1..64))
    [System.IO.File]::WriteAllBytes((Join-Path $fixtureRoot $portableName), [byte[]](65..128))
    $installerPath = Join-Path $fixtureRoot $installerName
    $runtimeArchivePath = Join-Path $fixtureRoot $runtimeArchiveName
    $runtimeManifestBytes = $utf8NoBom.GetBytes('{"schemaVersion":1,"runtimeVersion":"2026.07.04","platform":"windows-x64","tools":{}}')
    $runtimeArchiveStream = [System.IO.File]::Open(
        $runtimeArchivePath,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        $runtimeZip = [System.IO.Compression.ZipArchive]::new(
            $runtimeArchiveStream,
            [System.IO.Compression.ZipArchiveMode]::Create,
            $false
        )
        try {
            $runtimeManifestEntry = $runtimeZip.CreateEntry(
                'runtime-manifest.json',
                [System.IO.Compression.CompressionLevel]::Optimal
            )
            $runtimeManifestStream = $runtimeManifestEntry.Open()
            try {
                $runtimeManifestStream.Write($runtimeManifestBytes, 0, $runtimeManifestBytes.Length)
            } finally {
                $runtimeManifestStream.Dispose()
            }
        } finally {
            $runtimeZip.Dispose()
        }
    } finally {
        $runtimeArchiveStream.Dispose()
    }
    $installerHash = Get-LowerSha256 -Path $installerPath
    $runtimeHash = Get-LowerSha256 -Path $runtimeArchivePath
    $runtimeManifestHash = [Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($runtimeManifestBytes)
    ).ToLowerInvariant()

    Write-Utf8Json -Path (Join-Path $fixtureRoot $manifestName) -Value ([ordered]@{
        schemaVersion = 1
        keyId = $keyId
        version = $version
        platform = 'windows-x86_64'
        publishedAt = '2026-08-17T12:00:00Z'
        installer = [ordered]@{
            fileName = $installerName
            size = [long](Get-Item -LiteralPath $installerPath).Length
            sha256 = $installerHash
        }
    })
    $manifestFixturePath = Join-Path $fixtureRoot $manifestName
    Push-Location (Join-Path $repositoryRoot 'nuclear-app')
    $previousPrivateKeyPath = $env:TAURI_SIGNING_PRIVATE_KEY_PATH
    $previousPrivateKey = $env:TAURI_SIGNING_PRIVATE_KEY
    $previousPrivateKeyPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    try {
        $env:TAURI_SIGNING_PRIVATE_KEY_PATH = $testPrivateKeyPath
        $env:TAURI_SIGNING_PRIVATE_KEY = $null
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $testKeyPassword
        & npm.cmd exec tauri signer sign -- $manifestFixturePath *> $null
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not sign the disposable app-manifest fixture.'
        }
    } finally {
        $env:TAURI_SIGNING_PRIVATE_KEY_PATH = $previousPrivateKeyPath
        $env:TAURI_SIGNING_PRIVATE_KEY = $previousPrivateKey
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousPrivateKeyPassword
        Pop-Location
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureRoot $legacyName),
        "$installerHash  $installerName`n",
        [System.Text.Encoding]::ASCII
    )
    Write-Utf8Json -Path (Join-Path $fixtureRoot $runtimeDescriptorName) -Value ([ordered]@{
        schemaVersion = 1
        keyId = $keyId
        runtimeVersion = $runtimeVersion
        platform = 'windows-x64'
        archiveName = $runtimeArchiveName
        compressedSize = [long](Get-Item -LiteralPath $runtimeArchivePath).Length
        sha256 = $runtimeHash
        manifestSha256 = $runtimeManifestHash
    })
    $runtimeDescriptorFixturePath = Join-Path $fixtureRoot $runtimeDescriptorName
    Push-Location (Join-Path $repositoryRoot 'nuclear-app')
    $previousPrivateKeyPath = $env:TAURI_SIGNING_PRIVATE_KEY_PATH
    $previousPrivateKey = $env:TAURI_SIGNING_PRIVATE_KEY
    $previousPrivateKeyPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    try {
        $env:TAURI_SIGNING_PRIVATE_KEY_PATH = $testPrivateKeyPath
        $env:TAURI_SIGNING_PRIVATE_KEY = $null
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $testKeyPassword
        & npm.cmd exec tauri signer sign -- $runtimeDescriptorFixturePath *> $null
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not sign the disposable runtime-descriptor fixture.'
        }
    } finally {
        $env:TAURI_SIGNING_PRIVATE_KEY_PATH = $previousPrivateKeyPath
        $env:TAURI_SIGNING_PRIVATE_KEY = $previousPrivateKey
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousPrivateKeyPassword
        Pop-Location
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureRoot "$runtimeArchiveName.sha256"),
        "$runtimeHash  $runtimeArchiveName`n",
        [System.Text.Encoding]::ASCII
    )

    $publicBeforeSums = @(Get-ChildItem -LiteralPath $fixtureRoot -File | Sort-Object -Property Name)
    $sumLines = foreach ($file in $publicBeforeSums) {
        "$(Get-LowerSha256 -Path $file.FullName)  $($file.Name)"
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $fixtureRoot 'SHA256SUMS'),
        (($sumLines -join "`n") + "`n"),
        [System.Text.Encoding]::ASCII
    )

    $inventoryFiles = @(Get-ChildItem -LiteralPath $fixtureRoot -File | Sort-Object -Property Name)
    $inventoryAssets = foreach ($file in $inventoryFiles) {
        [ordered]@{
            fileName = $file.Name
            size = [long]$file.Length
            sha256 = Get-LowerSha256 -Path $file.FullName
        }
    }
    Write-Utf8Json -Depth 10 -Path (Join-Path $fixtureRoot 'release-candidate-inventory.json') -Value ([ordered]@{
        schemaVersion = 1
        releaseVersion = $version
        releaseTag = "v$version"
        platform = 'windows-x86_64'
        keyId = $keyId
        sourceCommit = ('a' * 40)
        createdAt = '2026-08-17T12:01:00Z'
        toolchains = [ordered]@{
            node = 'v22.23.1'
            npm = '10.9.9'
            rustc = 'rustc 1.94.1 (fixture)'
            cargo = 'cargo 1.94.1 (fixture)'
        }
        assets = @($inventoryAssets)
    })

    $verifyScript = Join-Path $repositoryRoot 'scripts\verify-release-candidate.ps1'
    $verificationOutput = @(
        & $verifyScript `
            -CandidateDirectory $fixtureRoot `
            -ExpectedVersion $version `
            -ExpectedCommitSha ('a' * 40) `
            -CurrentKeyId $keyId `
            -CurrentPublicKey $testPublicKey
    )
    Assert-True -Condition ($verificationOutput.Count -eq 1 -and $verificationOutput[0] -match '^Verified Nuclear Downloader 0\.6\.0 candidate') -Message 'Valid fixture candidate did not pass verification.'

    $manifestSignaturePath = Join-Path $fixtureRoot "$manifestName.sig"
    $validManifestSignature = [System.IO.File]::ReadAllText($manifestSignaturePath, $utf8NoBom)
    $decodedManifestSignature = $utf8NoBom.GetString(
        [Convert]::FromBase64String($validManifestSignature.Trim())
    )
    $signatureLines = @($decodedManifestSignature -split "`n")
    Assert-True -Condition ($signatureLines.Count -ge 4) -Message 'Tauri test signature did not contain the expected four Minisign lines.'
    $signedLine = $signatureLines[1].TrimEnd("`r")
    $replacementCharacter = if ($signedLine[20] -ceq 'A') { 'B' } else { 'A' }
    $signatureLines[1] = $signedLine.Substring(0, 20) + $replacementCharacter + $signedLine.Substring(21)
    $tamperedInnerSignature = $signatureLines[0..3] -join "`n"
    $tamperedTauriSignature = [Convert]::ToBase64String(
        $utf8NoBom.GetBytes($tamperedInnerSignature)
    )
    [System.IO.File]::WriteAllText(
        $manifestSignaturePath,
        $tamperedTauriSignature,
        $utf8NoBom
    )
    $badSignatureRejected = $false
    try {
        & $verifyScript `
            -CandidateDirectory $fixtureRoot `
            -ExpectedVersion $version `
            -ExpectedCommitSha ('a' * 40) `
            -CurrentKeyId $keyId `
            -CurrentPublicKey $testPublicKey *> $null
    } catch {
        $badSignatureRejected = $true
    }
    Assert-True -Condition $badSignatureRejected -Message 'Candidate verification did not reject an invalid detached signature.'
    [System.IO.File]::WriteAllText($manifestSignaturePath, $validManifestSignature, $utf8NoBom)

    [System.IO.File]::AppendAllText($installerPath, 'tamper', [System.Text.Encoding]::ASCII)
    $tamperRejected = $false
    try {
        & $verifyScript `
            -CandidateDirectory $fixtureRoot `
            -ExpectedVersion $version `
            -ExpectedCommitSha ('a' * 40) `
            -CurrentKeyId $keyId `
            -CurrentPublicKey $testPublicKey *> $null
    } catch {
        $tamperRejected = $true
    }
    Assert-True -Condition $tamperRejected -Message 'Candidate verification did not reject a tampered installer.'

    Write-Output 'Packaging contract parser and fixture tests passed.'
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $canonicalTest = [System.IO.Path]::GetFullPath($testRoot)
        $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalTest)
        if (-not [System.IO.Path]::IsPathRooted($relative) -and
            $relative -notmatch '^\.\.' -and
            (Test-Path -LiteralPath $testMarker -PathType Leaf) -and
            (Get-Content -Raw -LiteralPath $testMarker) -ceq 'owned') {
            Remove-Item -LiteralPath $canonicalTest -Recurse -Force
        } else {
            Write-Warning "Temporary packaging-test cleanup was skipped: $canonicalTest"
        }
    }
}
