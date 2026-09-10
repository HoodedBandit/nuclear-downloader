$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

& (Join-Path $PSScriptRoot 'test-publish-contracts.ps1')

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
$rustSourceRoot = Join-Path $repositoryRoot 'nuclear-app\src-tauri'
$rustSourceFiles = @(
    Get-ChildItem -LiteralPath (Join-Path $rustSourceRoot 'src') -Recurse -File -Filter '*.rs'
    Get-Item -LiteralPath (Join-Path $rustSourceRoot 'build.rs')
    Get-Item -LiteralPath (Join-Path $rustSourceRoot 'build_config.rs')
)
$webdriverRustSource = @($rustSourceFiles | Where-Object {
    (Get-Content -Raw -LiteralPath $_.FullName) -match 'tauri_plugin_wdio'
})
if ($cargoToml -match 'tauri-plugin-wdio' -or $webdriverRustSource.Count -ne 0) {
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
$ciWorkflow = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot '.github\workflows\ci.yml')
$backendSourceGateContracts = @(
    'Verify backend source architecture',
    'python -m unittest scripts/test_inventory_backend_methods.py',
    'python -m unittest scripts/test_backend_architecture.py',
    'python scripts/check-backend-architecture.py',
    'python scripts/inventory-backend-methods.py check'
)
foreach ($required in $backendSourceGateContracts) {
    if (-not $ciWorkflow.Contains($required) -or -not $candidateWorkflow.Contains($required)) {
        throw "CI and release-candidate workflows must both enforce the backend source contract: $required"
    }
}
foreach ($required in @(
    'cargo install tauri-driver --version 2.0.6 --locked',
    'run-windows-candidate-acceptance-user.ps1',
    'test-windows-user-process.ps1',
    'Test release evidence contracts before building',
    'test:e2e:production-bundle',
    '-ExpectedCandidateRunId ''${{ github.run_id }}''',
    'NUCLEAR_E2E_YOUTUBE_FIXTURE_URL: ${{ secrets.NUCLEAR_E2E_YOUTUBE_FIXTURE_URL }}',
    'NUCLEAR_E2E_YOUTUBE_FIXTURE_ID: ${{ vars.NUCLEAR_E2E_YOUTUBE_FIXTURE_ID }}',
    'NUCLEAR_E2E_X_FIXTURE_URL: ${{ secrets.NUCLEAR_E2E_X_FIXTURE_URL }}',
    'NUCLEAR_E2E_X_FIXTURE_ID: ${{ vars.NUCLEAR_E2E_X_FIXTURE_ID }}',
    '- name: Upload sanitized acceptance evidence',
    'if: ${{ always() }}',
    'if-no-files-found: warn'
)) {
    if (-not $candidateWorkflow.Contains($required)) {
        throw "Release-candidate workflow is missing required acceptance contract: $required"
    }
}

$acceptanceScript = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'scripts\run-windows-candidate-acceptance.ps1')
$acceptancePrivacyScriptPath = Join-Path $repositoryRoot 'scripts\acceptance-log-privacy.ps1'
$acceptancePrivacyScript = Get-Content -Raw -LiteralPath $acceptancePrivacyScriptPath
$acceptanceUninstallScriptPath = Join-Path $repositoryRoot 'scripts\acceptance-uninstall.ps1'
foreach ($required in @('function Protect-AcceptanceLog', 'function Publish-SafeAcceptanceEvidence', 'Test-ByteSequence')) {
    if (-not $acceptancePrivacyScript.Contains($required)) {
        throw "Acceptance evidence privacy helper is missing required contract: $required"
    }
}
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
    '. (Join-Path $PSScriptRoot ''release-json.ps1'')',
    '$inventory = Read-BoundedReleaseJson -Path $inventoryPath -Limit 1MB',
    '$wdioEnvironment.NUCLEAR_E2E_NATIVE_DRIVER_PATH = $edgeDriverPath',
    'function Stop-NewWebDriverProcesses',
    '$process.Kill($true)',
    '$actualPath -cne $ExpectedNativeDriverPath',
    'function Invoke-WdioSuite',
    '$start.RedirectStandardOutput = $true',
    '$start.RedirectStandardError = $true',
    '. (Join-Path $PSScriptRoot ''acceptance-log-privacy.ps1'')',
    '$resultsRoot = Join-Path $acceptanceRoot ''results-staging''',
    'Protect-AcceptanceLog',
    'Publish-SafeAcceptanceEvidence',
    '$protectedFixtureUrls.Add($fixtureUrl)',
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
    (Join-Path $repositoryRoot 'scripts\verify-manual-acceptance-evidence.ps1'),
    (Join-Path $repositoryRoot 'scripts\write-windows-manual-acceptance.ps1'),
    (Join-Path $repositoryRoot 'scripts\release-json.ps1'),
    $acceptancePrivacyScriptPath,
    $acceptanceUninstallScriptPath,
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

. $acceptancePrivacyScriptPath
. $acceptanceUninstallScriptPath
$uninstallFixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-uninstall-contract-$([Guid]::NewGuid().ToString('N'))"
try {
    New-Item -ItemType Directory -Path $uninstallFixtureRoot | Out-Null
    $journalPath = Join-Path $uninstallFixtureRoot 'state-v1.dpapi'
    [System.IO.File]::WriteAllBytes($journalPath, [byte[]](1, 2, 3, 4))
    $fingerprint = Get-AcceptanceFileFingerprint -Path $journalPath

    $residualInstallRoot = Join-Path $uninstallFixtureRoot 'residual-install'
    New-Item -ItemType Directory -Path $residualInstallRoot | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $residualInstallRoot 'left-behind.dll'), 'residue')
    $residueRejected = $false
    try {
        Wait-AcceptanceInstallRootRemoved -InstallRoot $residualInstallRoot `
            -TimeoutMilliseconds 10 -PollMilliseconds 1
    } catch {
        $residueRejected = $true
    }
    if (-not $residueRejected) {
        throw 'Uninstall acceptance did not reject residue after the application executable disappeared.'
    }

    [System.IO.File]::WriteAllBytes($journalPath, [byte[]](1, 2))
    $changedJournalRejected = $false
    try {
        Assert-AcceptanceFileUnchanged -Path $journalPath -Before $fingerprint
    } catch {
        $changedJournalRejected = $true
    }
    if (-not $changedJournalRejected) {
        throw 'Uninstall acceptance did not reject a truncated retained journal.'
    }

    [System.IO.File]::WriteAllBytes($journalPath, [byte[]](4, 3, 2, 1))
    $replacedJournalRejected = $false
    try {
        Assert-AcceptanceFileUnchanged -Path $journalPath -Before $fingerprint
    } catch {
        $replacedJournalRejected = $true
    }
    if (-not $replacedJournalRejected) {
        throw 'Uninstall acceptance did not reject a same-sized replacement journal.'
    }

    [System.IO.File]::WriteAllBytes($journalPath, [byte[]](1, 2, 3, 4))
    $removedInstallRoot = Join-Path $uninstallFixtureRoot 'removed-install'
    New-Item -ItemType Directory -Path $removedInstallRoot | Out-Null
    Remove-Item -LiteralPath $removedInstallRoot -Force
    Wait-AcceptanceInstallRootRemoved -InstallRoot $removedInstallRoot `
        -TimeoutMilliseconds 10 -PollMilliseconds 1
    Assert-AcceptanceFileUnchanged -Path $journalPath -Before $fingerprint
} finally {
    foreach ($relativeFile in @('state-v1.dpapi', 'residual-install\left-behind.dll')) {
        $fixtureFile = Join-Path $uninstallFixtureRoot $relativeFile
        if (Test-Path -LiteralPath $fixtureFile) { Remove-Item -LiteralPath $fixtureFile -Force }
    }
    foreach ($relativeDirectory in @('residual-install', 'removed-install')) {
        $fixtureDirectory = Join-Path $uninstallFixtureRoot $relativeDirectory
        if (Test-Path -LiteralPath $fixtureDirectory) {
            [System.IO.Directory]::Delete($fixtureDirectory, $false)
        }
    }
    if (Test-Path -LiteralPath $uninstallFixtureRoot) {
        [System.IO.Directory]::Delete($uninstallFixtureRoot, $false)
    }
}
$privacyFixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-log-privacy-$([Guid]::NewGuid().ToString('N'))"
try {
    $privacyUtf8 = [System.Text.UTF8Encoding]::new($false)
    $youtubeUrl = 'https://www.youtube.com/watch?v=private-fixture-token'
    $xUrl = 'https://x.com/private-fixture-status/12345'
    $protectedUrls = @($youtubeUrl, $xUrl)

    foreach ($caseName in @('success', 'failure')) {
        $staging = Join-Path $privacyFixtureRoot "$caseName-staging"
        $published = Join-Path $privacyFixtureRoot "$caseName-published"
        New-Item -ItemType Directory -Path $staging | Out-Null
        [System.IO.File]::WriteAllText(
            (Join-Path $staging '01-wdio-fixture.stdout.log'),
            "configured URL: $youtubeUrl`n",
            $privacyUtf8
        )
        [System.IO.File]::WriteAllText(
            (Join-Path $staging '01-wdio-fixture.stderr.log'),
            "configured URL: $xUrl`n",
            $privacyUtf8
        )
        if ($caseName -ceq 'failure') {
            try {
                throw 'Simulated acceptance failure before evidence finalization.'
            } catch {
                # The production runner finalizes retained diagnostics from its
                # outer finally block after the primary acceptance failure.
            } finally {
                Publish-SafeAcceptanceEvidence -StagingDirectory $staging `
                    -PublishedDirectory $published -ProtectedValues $protectedUrls
            }
        } else {
            Publish-SafeAcceptanceEvidence -StagingDirectory $staging `
                -PublishedDirectory $published -ProtectedValues $protectedUrls
        }
        foreach ($retained in @(Get-ChildItem -LiteralPath $published -File)) {
            $retainedBytes = [System.IO.File]::ReadAllBytes($retained.FullName)
            $retainedText = $privacyUtf8.GetString($retainedBytes)
            if (-not $retainedText.Contains('[protected-fixture-url]')) {
                throw "$caseName evidence finalization did not retain a redacted diagnostic marker."
            }
            foreach ($url in $protectedUrls) {
                if (Test-ByteSequence -Content $retainedBytes -Sequence $privacyUtf8.GetBytes($url)) {
                    throw "Protected fixture URL bytes survived $caseName evidence finalization."
                }
            }
        }
    }

    $unsafeStaging = Join-Path $privacyFixtureRoot 'unsafe-staging'
    $unsafePublished = Join-Path $privacyFixtureRoot 'unsafe-published'
    New-Item -ItemType Directory -Path $unsafeStaging | Out-Null
    [System.IO.File]::WriteAllBytes(
        (Join-Path $unsafeStaging '01-wdio-fixture.stdout.log'),
        [byte[]]@(0xff, 0xfe, 0xfd)
    )
    $redactionRejected = $false
    try {
        Publish-SafeAcceptanceEvidence -StagingDirectory $unsafeStaging `
            -PublishedDirectory $unsafePublished -ProtectedValues $protectedUrls
    } catch {
        $redactionRejected = $true
    }
    if (-not $redactionRejected -or (Test-Path -LiteralPath $unsafePublished)) {
        throw 'A redaction failure left an uploadable acceptance evidence directory.'
    }
} finally {
    $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $canonicalPrivacyFixture = [System.IO.Path]::GetFullPath($privacyFixtureRoot)
    $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalPrivacyFixture)
    if ($relative -match '^nuclear-log-privacy-[0-9a-f]{32}$' -and
        (Test-Path -LiteralPath $canonicalPrivacyFixture)) {
        Remove-Item -LiteralPath $canonicalPrivacyFixture -Recurse -Force
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
        releaseVersion = '0.7.1'
        releaseTag = 'v0.7.1'
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
        genericPlaylistValidated = 'passed'
        fixtureServer = 'passed'
        cleanInstall = 'passed'
        installedMp4RetryCollisionPlaylistCancelAllRuntimeChecks = 'passed'
        forcedActiveProcessTermination = 'passed'
        interruptedOperationRestartRecovery = 'passed'
        portableStartup = 'passed'
        uninstallAndRetainedUserData = 'passed'
        postAcceptanceHashVerification = 'passed'
    }
    $evidence = [ordered]@{
        schemaVersion = 1
        releaseVersion = '0.7.1'
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
        controlledSiteFixtures = [ordered]@{
            passed = @()
            missing = @(
                'youtube-maintainer-fixture',
                'x-maintainer-fixture'
            )
        }
        extractorQualificationStatus = 'incomplete'
        qualificationStatus = 'incomplete'
        incompleteRequirements = @(
            'clean-windows11-installer',
            'clean-windows11-portable',
            'youtube-maintainer-fixture',
            'x-maintainer-fixture',
            'dedicated-account-cookie-login',
            'signed-app-update',
            'signed-runtime-update-rollback'
        )
        manualAcceptanceRequired = @(
            'clean-windows11-installer',
            'clean-windows11-portable',
            'youtube-maintainer-fixture',
            'x-maintainer-fixture',
            'dedicated-account-cookie-login',
            'signed-app-update',
            'signed-runtime-update-rollback'
        )
        candidateAssets = @($asset)
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText(
        (Join-Path $candidateFixture 'release-candidate-inventory.json'),
        ($inventory | ConvertTo-Json -Depth 10),
        $utf8
    )
    # Exercise the production reader -> evidence serialization -> validator path,
    # not only hand-written fixture strings. Date coercion previously broke it.
    . (Join-Path $PSScriptRoot 'release-json.ps1')
    $originalCulture = [System.Threading.Thread]::CurrentThread.CurrentCulture
    try {
        foreach ($culture in @('en-US', 'de-DE')) {
            [System.Threading.Thread]::CurrentThread.CurrentCulture = [System.Globalization.CultureInfo]::GetCultureInfo($culture)
            $loadedInventory = Read-BoundedReleaseJson -Path (Join-Path $candidateFixture 'release-candidate-inventory.json') -Limit 1MB
            if ($loadedInventory.createdAt -isnot [string] -or
                $loadedInventory.createdAt -cne '2026-08-18T12:00:00Z') {
                throw 'The production inventory reader changed a canonical timestamp.'
            }
            $evidence.candidateCreatedAt = $loadedInventory.createdAt
            $roundTrip = ($evidence | ConvertTo-Json -Depth 10) | ConvertFrom-Json -DateKind String
            if ($roundTrip.candidateCreatedAt -cne $loadedInventory.createdAt) {
                throw 'Evidence serialization changed the candidate timestamp.'
            }
        }
    } finally {
        [System.Threading.Thread]::CurrentThread.CurrentCulture = $originalCulture
    }
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

    $partiallyConfiguredEvidence = ConvertFrom-Json -InputObject ($evidence | ConvertTo-Json -Depth 10) -DateKind String
    $partiallyConfiguredEvidence.controlledSiteFixtures.passed = @(
        [ordered]@{ caseId = 'youtube-maintainer-fixture'; fixtureId = 'youtube-controlled-01' }
    )
    $partiallyConfiguredEvidence.controlledSiteFixtures.missing = @('x-maintainer-fixture')
    [System.IO.File]::WriteAllText($evidencePath, ($partiallyConfiguredEvidence | ConvertTo-Json -Depth 10), $utf8)
    & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
        -EvidenceDirectory $evidenceFixture `
        -CandidateDirectory $candidateFixture `
        -ExpectedCommitSha ('b' * 40) `
        -ExpectedCandidateRunId '12345' *> $null

    $fullyConfiguredEvidence = ConvertFrom-Json -InputObject ($evidence | ConvertTo-Json -Depth 10) -DateKind String
    $fullyConfiguredEvidence.controlledSiteFixtures.passed = @(
        [ordered]@{ caseId = 'youtube-maintainer-fixture'; fixtureId = 'youtube-controlled-01' },
        [ordered]@{ caseId = 'x-maintainer-fixture'; fixtureId = 'x-controlled-01' }
    )
    $fullyConfiguredEvidence.controlledSiteFixtures.missing = @()
    $fullyConfiguredEvidence.extractorQualificationStatus = 'complete'
    [System.IO.File]::WriteAllText($evidencePath, ($fullyConfiguredEvidence | ConvertTo-Json -Depth 10), $utf8)
    & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
        -EvidenceDirectory $evidenceFixture `
        -CandidateDirectory $candidateFixture `
        -ExpectedCommitSha ('b' * 40) `
        -ExpectedCandidateRunId '12345' *> $null
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)

    function Assert-EvidenceRejected {
        $rejected = $false
        try {
            & (Join-Path $repositoryRoot 'scripts\verify-acceptance-evidence.ps1') `
                -EvidenceDirectory $evidenceFixture -CandidateDirectory $candidateFixture `
                -ExpectedCommitSha ('b' * 40) -ExpectedCandidateRunId '12345' *> $null
        } catch { $rejected = $true }
        if (-not $rejected) { throw 'Malformed acceptance evidence was accepted.' }
    }
    $evidence.candidateCreatedAt = '08/18/2026 12:00:00'
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.candidateCreatedAt = $loadedInventory.createdAt
    $evidence.webdriver.integrityRid = 12288
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.webdriver.integrityRid = 8192
    $evidence.qualificationStatus = 'complete'
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.qualificationStatus = 'incomplete'
    $evidence.extractorQualificationStatus = 'complete'
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.extractorQualificationStatus = 'incomplete'
    $evidence.controlledSiteFixtures.passed = @(
        [ordered]@{ caseId = 'youtube-maintainer-fixture'; fixtureId = 'https://example.invalid/watch' }
    )
    $evidence.controlledSiteFixtures.missing = @('x-maintainer-fixture')
    [System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 10), $utf8)
    Assert-EvidenceRejected
    $evidence.controlledSiteFixtures.passed = @()
    $evidence.controlledSiteFixtures.missing = @(
        'youtube-maintainer-fixture',
        'x-maintainer-fixture'
    )
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

$manualFixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "nuclear-manual-evidence-$([Guid]::NewGuid().ToString('N'))"
$manualCandidate = Join-Path $manualFixtureRoot 'candidate'
$manualInputDirectory = Join-Path $manualFixtureRoot 'input'
$manualOutputDirectory = Join-Path $manualFixtureRoot 'output'
try {
    New-Item -ItemType Directory -Path $manualCandidate, $manualInputDirectory, $manualOutputDirectory | Out-Null
    $manualUtf8 = [System.Text.UTF8Encoding]::new($false)
    $manualCommit = 'b' * 40
    $manualRunId = '12345'
    $candidateCreatedAt = [DateTimeOffset]::UtcNow.AddHours(-1).ToString('yyyy-MM-ddTHH:mm:ssZ')
    $caseCompletedAt = [DateTimeOffset]::UtcNow.AddMinutes(-1).ToString('yyyy-MM-ddTHH:mm:ssZ')
    $installerName = 'Nuclear.Downloader_0.7.1_x64-setup.exe'
    $portableName = 'Nuclear.Downloader_0.7.1_x64-portable.zip'
    $appManifestName = 'nuclear-downloader-v0.7.1-update.json'
    $runtimeDescriptorName = 'nuclear-downloader-runtime-windows-x64.json'
    $runtimeArchiveName = 'nuclear-downloader-runtime-1.2.3-windows-x64.zip'
    $assetHashes = [ordered]@{
        $installerName = '1' * 64
        $portableName = '2' * 64
        $appManifestName = '3' * 64
        $runtimeDescriptorName = '4' * 64
        $runtimeArchiveName = '5' * 64
    }
    $manualAssets = @($assetHashes.GetEnumerator() | ForEach-Object {
        [ordered]@{ fileName = [string]$_.Key; size = [long]3; sha256 = [string]$_.Value }
    })
    $manualInventory = [ordered]@{
        schemaVersion = 1
        releaseVersion = '0.7.1'
        releaseTag = 'v0.7.1'
        platform = 'windows-x86_64'
        keyId = 'fixture-key'
        sourceCommit = $manualCommit
        createdAt = $candidateCreatedAt
        toolchains = [ordered]@{ node = 'fixture'; npm = 'fixture'; rustc = 'fixture'; cargo = 'fixture' }
        assets = $manualAssets
    }
    $manualInventoryPath = Join-Path $manualCandidate 'release-candidate-inventory.json'
    [System.IO.File]::WriteAllText($manualInventoryPath, ($manualInventory | ConvertTo-Json -Depth 10), $manualUtf8)

    $runtimeDescriptor = [ordered]@{
        schemaVersion = 1
        keyId = 'fixture-key'
        runtimeVersion = '1.2.3'
        platform = 'windows-x64'
        archiveName = $runtimeArchiveName
        compressedSize = [long]3
        sha256 = '5' * 64
        manifestSha256 = '6' * 64
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $manualCandidate $runtimeDescriptorName),
        ($runtimeDescriptor | ConvertTo-Json -Depth 10 -Compress),
        $manualUtf8
    )
    $runtimeStage = Join-Path $manualFixtureRoot 'runtime-stage'
    New-Item -ItemType Directory -Path $runtimeStage | Out-Null
    $runtimeManifest = [ordered]@{
        schemaVersion = 1
        runtimeVersion = '1.2.3'
        platform = 'windows-x64'
        tools = @(
            [ordered]@{ name = 'yt-dlp'; version = '2026.07.04'; path = 'yt-dlp.exe'; sha256 = 'a' * 64 }
            [ordered]@{ name = 'ffmpeg'; version = 'ffmpeg version 8.1'; path = 'ffmpeg.exe'; sha256 = 'b' * 64 }
            [ordered]@{ name = 'ffprobe'; version = 'ffprobe version 8.1'; path = 'ffprobe.exe'; sha256 = 'c' * 64 }
            [ordered]@{ name = 'deno'; version = 'deno 2.9.2'; path = 'deno.exe'; sha256 = 'd' * 64 }
        )
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $runtimeStage 'runtime-manifest.json'),
        ($runtimeManifest | ConvertTo-Json -Depth 10),
        $manualUtf8
    )
    Compress-Archive -LiteralPath (Join-Path $runtimeStage 'runtime-manifest.json') `
        -DestinationPath (Join-Path $manualCandidate $runtimeArchiveName)

    $inventoryDigest = (Get-FileHash -LiteralPath $manualInventoryPath -Algorithm SHA256).Hash.ToLowerInvariant()
    function New-ManualCase {
        param([string] $CaseId, [object] $Details)
        return [ordered]@{
            caseId = $CaseId
            outcome = 'passed'
            operator = 'fixture-operator'
            completedAt = $caseCompletedAt
            candidateInventorySha256 = $inventoryDigest
            details = $Details
        }
    }
    $manualCases = @(
        (New-ManualCase 'clean-windows11-installer' ([ordered]@{
            artifactFileName = $installerName; artifactSha256 = $assetHashes[$installerName]
        }))
        (New-ManualCase 'clean-windows11-portable' ([ordered]@{
            artifactFileName = $portableName; artifactSha256 = $assetHashes[$portableName]
        }))
        (New-ManualCase 'youtube-maintainer-fixture' ([ordered]@{
            artifactFileName = $installerName; artifactSha256 = $assetHashes[$installerName]; fixtureId = 'youtube-controlled-01'
        }))
        (New-ManualCase 'x-maintainer-fixture' ([ordered]@{
            artifactFileName = $installerName; artifactSha256 = $assetHashes[$installerName]; fixtureId = 'x-controlled-01'
        }))
        (New-ManualCase 'dedicated-account-cookie-login' ([ordered]@{
            artifactFileName = $installerName; artifactSha256 = $assetHashes[$installerName]; fixtureId = 'cookie-controlled-01'
        }))
        (New-ManualCase 'signed-app-update' ([ordered]@{
            fromVersion = '0.6.0'; toVersion = '0.7.1'
            manifestSha256 = $assetHashes[$appManifestName]; installerSha256 = $assetHashes[$installerName]
        }))
        (New-ManualCase 'signed-runtime-update-rollback' ([ordered]@{
            fromVersion = '1.2.2'; toVersion = '1.2.3'; rollbackVersion = '1.2.2'
            descriptorSha256 = $assetHashes[$runtimeDescriptorName]; archiveSha256 = $assetHashes[$runtimeArchiveName]
        }))
    )
    $manualInput = [ordered]@{
        schemaVersion = 1
        clientEnvironment = [ordered]@{
            installationType = 'Client'
            productName = 'Windows 11 Pro'
            displayVersion = '24H2'
            buildNumber = [long]26100
            updateBuildRevision = [long]4946
            architecture = 'X64'
            webView2RuntimeVersion = '151.0.4129.101'
            applicationVersion = '0.7.1'
            managedRuntimeVersion = '1.2.3'
            runtimeToolVersions = [ordered]@{
                ytDlp = '2026.07.04'
                ffmpeg = 'ffmpeg version 8.1'
                ffprobe = 'ffprobe version 8.1'
                deno = 'deno 2.9.2'
            }
        }
        cases = $manualCases
    }
    $manualInputPath = Join-Path $manualInputDirectory 'manual-input.json'
    [System.IO.File]::WriteAllText($manualInputPath, ($manualInput | ConvertTo-Json -Depth 12), $manualUtf8)
    & (Join-Path $repositoryRoot 'scripts\write-windows-manual-acceptance.ps1') `
        -CandidateDirectory $manualCandidate `
        -InputPath $manualInputPath `
        -OutputDirectory $manualOutputDirectory `
        -ExpectedCommitSha $manualCommit `
        -ExpectedCandidateRunId $manualRunId `
        -Submitter 'fixture-submitter' *> $null
    $manualEvidencePath = Join-Path $manualOutputDirectory 'windows-x64-manual-acceptance.json'
    & (Join-Path $repositoryRoot 'scripts\verify-manual-acceptance-evidence.ps1') `
        -EvidencePath $manualEvidencePath `
        -CandidateDirectory $manualCandidate `
        -ExpectedCommitSha $manualCommit `
        -ExpectedCandidateRunId $manualRunId `
        -ExpectedSubmitter 'fixture-submitter' *> $null
    $manualBaselineJson = [System.IO.File]::ReadAllText($manualEvidencePath, $manualUtf8)

    function Assert-ManualEvidenceRejected {
        param([Parameter(Mandatory)] [scriptblock] $Mutate)
        $badEvidence = ConvertFrom-Json -InputObject $manualBaselineJson -DateKind String
        & $Mutate $badEvidence
        [System.IO.File]::WriteAllText($manualEvidencePath, ($badEvidence | ConvertTo-Json -Depth 12), $manualUtf8)
        $rejected = $false
        try {
            & (Join-Path $repositoryRoot 'scripts\verify-manual-acceptance-evidence.ps1') `
                -EvidencePath $manualEvidencePath `
                -CandidateDirectory $manualCandidate `
                -ExpectedCommitSha $manualCommit `
                -ExpectedCandidateRunId $manualRunId `
                -ExpectedSubmitter 'fixture-submitter' *> $null
        } catch {
            $rejected = $true
        }
        if (-not $rejected) {
            throw 'Malformed manual acceptance evidence was accepted.'
        }
    }

    Assert-ManualEvidenceRejected { param($value) $value.candidateAssets[0].sha256 = 'f' * 64 }
    Assert-ManualEvidenceRejected { param($value) $value.cases[0].candidateInventorySha256 = 'f' * 64 }
    Assert-ManualEvidenceRejected { param($value) $value.cases[1].caseId = $value.cases[0].caseId }
    Assert-ManualEvidenceRejected { param($value) $value.cases[2].outcome = 'failed' }
    Assert-ManualEvidenceRejected { param($value) $value.cases[3].operator = 'bad/operator' }
    Assert-ManualEvidenceRejected { param($value) $value.cases[4].completedAt = '2000-01-01T00:00:00Z' }
    Assert-ManualEvidenceRejected { param($value) $value.cases[2].details.fixtureId = 'https://example.invalid/watch' }
    Assert-ManualEvidenceRejected { param($value) $value.clientEnvironment.installationType = 'Server' }
    Assert-ManualEvidenceRejected { param($value) $value.clientEnvironment.buildNumber = [long]21999 }
    Assert-ManualEvidenceRejected { param($value) $value.clientEnvironment.webView2RuntimeVersion = 'not-a-version' }
    Assert-ManualEvidenceRejected { param($value) $value.clientEnvironment.managedRuntimeVersion = '9.9.9' }
    Assert-ManualEvidenceRejected { param($value) $value.cases[5].details.fromVersion = '0.7.2' }
    Assert-ManualEvidenceRejected { param($value) $value.cases[6].details.fromVersion = '1.2.4' }
    Assert-ManualEvidenceRejected { param($value) $value | Add-Member -NotePropertyName unexpected -NotePropertyValue $true }

    $unboundInput = ConvertFrom-Json -InputObject ([System.IO.File]::ReadAllText($manualInputPath, $manualUtf8)) -DateKind String
    $unboundInput.cases[0].candidateInventorySha256 = 'f' * 64
    $unboundInputPath = Join-Path $manualInputDirectory 'unbound-input.json'
    [System.IO.File]::WriteAllText($unboundInputPath, ($unboundInput | ConvertTo-Json -Depth 12), $manualUtf8)
    $unboundOutput = Join-Path $manualFixtureRoot 'unbound-output'
    New-Item -ItemType Directory -Path $unboundOutput | Out-Null
    $unboundRejected = $false
    try {
        & (Join-Path $repositoryRoot 'scripts\write-windows-manual-acceptance.ps1') `
            -CandidateDirectory $manualCandidate `
            -InputPath $unboundInputPath `
            -OutputDirectory $unboundOutput `
            -ExpectedCommitSha $manualCommit `
            -ExpectedCandidateRunId $manualRunId `
            -Submitter 'fixture-submitter' *> $null
    } catch {
        $unboundRejected = $true
    }
    if (-not $unboundRejected) {
        throw 'The manual evidence writer rebound an input case from another candidate.'
    }
} finally {
    $canonicalTemp = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $canonicalManualFixture = [System.IO.Path]::GetFullPath($manualFixtureRoot)
    $relative = [System.IO.Path]::GetRelativePath($canonicalTemp, $canonicalManualFixture)
    if ($relative -match '^nuclear-manual-evidence-[0-9a-f]{32}$' -and
        (Test-Path -LiteralPath $canonicalManualFixture)) {
        Remove-Item -LiteralPath $canonicalManualFixture -Recurse -Force
    }
}

Write-Output 'WebDriver and exact-candidate acceptance contracts passed.'
