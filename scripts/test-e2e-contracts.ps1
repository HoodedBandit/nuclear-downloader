$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$package = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'nuclear-app\package.json') | ConvertFrom-Json
$expectedPackages = [ordered]@{
    '@wdio/cli' = '9.30.1'
    '@wdio/globals' = '9.29.1'
    '@wdio/local-runner' = '9.30.1'
    '@wdio/mocha-framework' = '9.30.1'
    '@wdio/spec-reporter' = '9.30.1'
    '@wdio/tauri-service' = '1.2.0'
}
foreach ($entry in $expectedPackages.GetEnumerator()) {
    $actual = $package.devDependencies.PSObject.Properties[$entry.Key].Value
    if ([string]$actual -cne [string]$entry.Value) {
        throw "$($entry.Key) must be exactly pinned to $($entry.Value); found $actual."
    }
}
if ([string]$package.overrides.'@wdio/tauri-service'.'@wdio/native-utils' -cne '2.6.0') {
    throw '@wdio/tauri-service must override its broken 2.4.0 native-utils edge to exact 2.6.0.'
}

$cargoToml = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'nuclear-app\src-tauri\Cargo.toml')
$rustEntry = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'nuclear-app\src-tauri\src\lib.rs')
if ($cargoToml -match 'tauri-plugin-wdio' -or $rustEntry -match 'tauri_plugin_wdio') {
    throw 'A WebDriver plugin must never be compiled into the application.'
}

$nativeConfig = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'nuclear-app\e2e\wdio.native.conf.mjs')
if ($nativeConfig -notmatch "driverProvider:\s*'external'" -or
    $nativeConfig -notmatch 'launcher as TauriLauncher' -or
    $nativeConfig -notmatch 'TauriLauncher,\s*launcherOptions' -or
    $nativeConfig -match "\[\s*'@wdio/tauri-service'," -or
    $nativeConfig -notmatch 'autoInstallTauriDriver:\s*false' -or
    $nativeConfig -notmatch 'captureBackendLogs:\s*false' -or
    $nativeConfig -notmatch 'captureFrontendLogs:\s*false' -or
    $nativeConfig -notmatch 'NUCLEAR_E2E_WEBVIEW_DATA_FOLDER' -or
    $nativeConfig -notmatch 'NUCLEAR_E2E_NATIVE_DRIVER_PATH' -or
    $nativeConfig -notmatch 'autoDownloadEdgeDriver:\s*true' -or
    $nativeConfig -notmatch 'nativeDriverPath' -or
    $nativeConfig -notmatch 'userDataFolder:\s*webviewDataFolder') {
    throw 'Native WebDriver must use the pinned, external official tauri-driver.'
}

$candidateWorkflow = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot '.github\workflows\release-candidate.yml')
foreach ($required in @(
    'cargo install tauri-driver --version 2.0.6 --locked',
    'run-windows-candidate-acceptance-user.ps1',
    'test-windows-user-process.ps1',
    'test:e2e:production-bundle',
    '-ExpectedCandidateRunId ''${{ github.run_id }}''',
    '- name: Upload private acceptance evidence',
    'if: ${{ always() }}',
    'if-no-files-found: warn'
)) {
    if (-not $candidateWorkflow.Contains($required)) {
        throw "Release-candidate workflow is missing required acceptance contract: $required"
    }
}

$acceptanceScript = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'scripts\run-windows-candidate-acceptance.ps1')
$edgeDriverScriptPath = Join-Path $repositoryRoot 'scripts\windows-edgedriver.ps1'
$edgeDriverScript = Get-Content -Raw -LiteralPath $edgeDriverScriptPath
foreach ($required in @(
    '$env:GITHUB_ACTIONS -cne ''true''',
    '[Environment+SpecialFolder]::LocalApplicationData',
    '$persistentAppDataRoot = Join-Path $localDataRoot ''Nuclear Downloader''',
    '$managedAppDataRoot = Join-Path $localDataRoot ''NuclearDownloader''',
    '$webviewDataRoot = Join-Path $localDataRoot ''com.mrw.nuclear''',
    '$edgeDriverDataRoot = Join-Path $webviewDataRoot ''EBWebView''',
    '$wdioEnvironment.NUCLEAR_E2E_WEBVIEW_DATA_FOLDER = $edgeDriverDataRoot',
    '. (Join-Path $PSScriptRoot ''windows-edgedriver.ps1'')',
    '$wdioEnvironment.NUCLEAR_E2E_NATIVE_DRIVER_PATH = $edgeDriverPath',
    'function Stop-NewWebDriverProcesses',
    '$process.Kill($true)',
    '$actualPath -cne $ExpectedNativeDriverPath',
    'function Invoke-WdioSuite',
    '$start.RedirectStandardOutput = $true',
    '$start.RedirectStandardError = $true',
    'Limit-RetainedProcessLog',
    'Write-ProcessLogTail'
)) {
    if (-not $acceptanceScript.Contains($required)) {
        throw "Candidate acceptance must retain failed process diagnostics: $required"
    }
}
foreach ($required in @(
    'function Get-WebView2RuntimeVersion',
    'function Convert-VersionResponseToText',
    'function Install-CompatibleEdgeDriver',
    'LATEST_RELEASE_$majorVersion',
    'Get-AuthenticodeSignature -LiteralPath $driverPath',
    'O=Microsoft Corporation'
)) {
    if (-not $edgeDriverScript.Contains($required)) {
        throw "EdgeDriver provisioning is missing required contract: $required"
    }
}

. $edgeDriverScriptPath
$utf16Version = [byte[]](@([System.Text.Encoding]::Unicode.GetPreamble()) +
    @([System.Text.Encoding]::Unicode.GetBytes("151.0.4129.107`r`n")))
if ((Convert-VersionResponseToText -Content $utf16Version) -cne '151.0.4129.107') {
    throw 'EdgeDriver version parsing did not decode the Microsoft UTF-16 response.'
}
$utf8Version = [byte[]](@([System.Text.UTF8Encoding]::new($true).GetPreamble()) +
    @([System.Text.Encoding]::UTF8.GetBytes("151.0.4129.107`n")))
if ((Convert-VersionResponseToText -Content $utf8Version) -cne '151.0.4129.107') {
    throw 'EdgeDriver version parsing did not decode a UTF-8 BOM response.'
}
$oversizedVersionRejected = $false
try {
    [void](Convert-VersionResponseToText -Content ([byte[]]::new(1025)))
} catch {
    $oversizedVersionRejected = $true
}
if (-not $oversizedVersionRejected) {
    throw 'EdgeDriver version parsing did not reject an oversized response.'
}
foreach ($forbidden in @('USERPROFILE =', 'LOCALAPPDATA =', 'APPDATA =')) {
    if ($acceptanceScript.Contains($forbidden)) {
        throw "Candidate acceptance must not replace Windows profile variables: $forbidden"
    }
}

foreach ($scriptPath in @(
    (Join-Path $repositoryRoot 'scripts\run-windows-candidate-acceptance.ps1'),
    (Join-Path $repositoryRoot 'scripts\run-windows-candidate-acceptance-user.ps1'),
    (Join-Path $repositoryRoot 'scripts\run-windows-candidate-acceptance-worker.ps1'),
    (Join-Path $repositoryRoot 'scripts\test-windows-user-process.ps1'),
    (Join-Path $repositoryRoot 'scripts\fixtures\user-process.ps1'),
    $edgeDriverScriptPath,
    (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1'),
    $PSCommandPath
)) {
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile(
        $scriptPath,
        [ref]$tokens,
        [ref]$errors
    )
    if ($errors.Count -ne 0) {
        throw "PowerShell parsing failed for $scriptPath`: $($errors[0].Message)"
    }
}

$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-evidence-$([Guid]::NewGuid().ToString('N'))"
$candidateFixture = Join-Path $fixtureRoot 'candidate'
$evidenceFixture = Join-Path $fixtureRoot 'evidence'
try {
    New-Item -ItemType Directory -Path $candidateFixture, $evidenceFixture | Out-Null
    $asset = [ordered]@{ fileName = 'fixture.bin'; size = 3; sha256 = ('a' * 64) }
    $inventory = [ordered]@{
        schemaVersion = 1
        releaseVersion = '0.6.0'
        releaseTag = 'v0.6.0'
        platform = 'windows-x86_64'
        keyId = 'fixture-key'
        sourceCommit = ('b' * 40)
        createdAt = '2026-08-18T12:00:00Z'
        toolchains = [ordered]@{ node = 'fixture'; npm = 'fixture'; rustc = 'fixture'; cargo = 'fixture' }
        assets = @($asset)
    }
    $steps = [ordered]@{
        portableExtracted = 'passed'
        fixtureGenerated = 'passed'
        fixtureServer = 'passed'
        cleanInstall = 'passed'
        installedFixtureDownloadConversionCancelReloadDiagnostics = 'passed'
        processRestartJournalRecovery = 'passed'
        portableStartup = 'passed'
        uninstallAndRetainedUserData = 'passed'
        postAcceptanceHashVerification = 'passed'
    }
    $evidence = [ordered]@{
        schemaVersion = 1
        releaseVersion = '0.6.0'
        sourceCommit = ('b' * 40)
        candidateRunId = '12345'
        candidateCreatedAt = '2026-08-18T12:00:00Z'
        startedAt = '2026-08-18T12:01:00Z'
        completedAt = '2026-08-18T12:02:00Z'
        os = [ordered]@{ description = 'Windows fixture'; architecture = 'X64' }
        webdriver = [ordered]@{
            integrityRid = 8192
            webView2RuntimeVersion = '151.0.4129.101'
            edgeDriverVersion = '151.0.4129.107'
        }
        steps = $steps
        manualAcceptanceRequired = @(
            'Dedicated-account cookie/login test with no CI cookie secret',
            'Maintainer review of managed-runtime update and rollback UI against protected signed test assets',
            'Maintainer acceptance decision before publication'
        )
        candidateAssets = @($asset)
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText(
        (Join-Path $candidateFixture 'release-candidate-inventory.json'),
        ($inventory | ConvertTo-Json -Depth 10),
        $utf8
    )
    $evidencePath = Join-Path $evidenceFixture 'windows-x64-acceptance.json'
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    # Evidence uploads include bounded diagnostics as well as the signed-byte binding.
    $logFixture = Join-Path $evidenceFixture '03-wdio-full.stdout.log'
    [System.IO.File]::WriteAllText($logFixture, 'fixture process diagnostics', $utf8)
    & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
        -EvidenceDirectory $evidenceFixture `
        -CandidateDirectory $candidateFixture `
        -ExpectedCommitSha ('b' * 40) `
        -ExpectedCandidateRunId '12345' *> $null

    function Assert-EvidenceRejected {
        $rejected = $false
        try {
            & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
                -EvidenceDirectory $evidenceFixture -CandidateDirectory $candidateFixture `
                -ExpectedCommitSha ('b' * 40) -ExpectedCandidateRunId '12345' *> $null
        } catch { $rejected = $true }
        if (-not $rejected) { throw 'Malformed acceptance evidence was accepted.' }
    }
    $evidence.webdriver.integrityRid = 12288
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.webdriver.integrityRid = 8192
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    $unexpectedPath = Join-Path $evidenceFixture 'unexpected.exe'
    [System.IO.File]::WriteAllText($unexpectedPath, 'not allowed', $utf8)
    Assert-EvidenceRejected
    Remove-Item -LiteralPath $unexpectedPath
    $stream = [System.IO.File]::OpenWrite($logFixture)
    try { $stream.SetLength(4MB + 129) } finally { $stream.Dispose() }
    Assert-EvidenceRejected
    Remove-Item -LiteralPath $logFixture
    $nestedPath = Join-Path $evidenceFixture 'nested'
    New-Item -ItemType Directory -Path $nestedPath | Out-Null
    Assert-EvidenceRejected
    [System.IO.Directory]::Delete($nestedPath, $false)

    $evidence.webdriver.edgeDriverVersion = '150.0.4129.107'
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    $incompatibleDriverRejected = $false
    try {
        & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
            -EvidenceDirectory $evidenceFixture `
            -CandidateDirectory $candidateFixture `
            -ExpectedCommitSha ('b' * 40) `
            -ExpectedCandidateRunId '12345' *> $null
    } catch {
        $incompatibleDriverRejected = $true
    }
    if (-not $incompatibleDriverRejected) {
        throw 'Acceptance evidence verification did not reject an incompatible EdgeDriver.'
    }

    $evidence.webdriver.edgeDriverVersion = '151.0.4129.107'
    $evidence.candidateAssets[0].sha256 = ('c' * 64)
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    $tamperRejected = $false
    try {
        & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
            -EvidenceDirectory $evidenceFixture `
            -CandidateDirectory $candidateFixture `
            -ExpectedCommitSha ('b' * 40) `
            -ExpectedCandidateRunId '12345' *> $null
    } catch {
        $tamperRejected = $true
    }
    if (-not $tamperRejected) {
        throw 'Acceptance evidence verification did not reject a tampered asset hash.'
    }
} finally {
    $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $canonicalFixture = [System.IO.Path]::GetFullPath($fixtureRoot)
    $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalFixture)
    if ($relative -match '^nuclear-evidence-[0-9a-f]{32}$' -and
        (Test-Path -LiteralPath $canonicalFixture)) {
        Remove-Item -LiteralPath $canonicalFixture -Recurse -Force
    }
}

Write-Output 'WebDriver and exact-candidate acceptance contracts passed.'
