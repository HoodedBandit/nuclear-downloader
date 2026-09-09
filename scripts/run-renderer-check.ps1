[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateSet('workflows', 'performance', 'visual')] [string] $Suite,
    [Parameter(Mandatory)] [string] $ChromeBinary,
    [Parameter(Mandatory)] [string] $ChromeDriverBinary,
    [Parameter(Mandatory)] [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')] [string] $ExpectedBrowserVersion,
    [ValidateSet(1, 100, 1000)] [int] $QueueSize = 1000,
    [ValidateSet(100, 150)] [int] $ScalePercent = 100
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$nodePath = Join-Path $appRoot 'src-tauri/target/toolchains/node-v22.23.1-win-x64/node.exe'

function Relative-InputPath([string] $Path) {
    [IO.Path]::GetRelativePath($repositoryRoot, [IO.Path]::GetFullPath($Path)).Replace('\', '/')
}
function Regular-FileIdentity([string] $Path, [string] $Label) {
    $full = [IO.Path]::GetFullPath($Path)
    $item = Get-Item -LiteralPath $full -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a regular non-reparse file: $full"
    }
    [ordered]@{
        path = $full
        size = [long] $item.Length
        sha256 = (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Sort-OrdinalUnique([string[]] $Values) {
    $ordered = [string[]] @($Values)
    [Array]::Sort($ordered, [StringComparer]::Ordinal)
    $result = [Collections.Generic.List[string]]::new()
    foreach ($value in $ordered) {
        if ($result.Count -eq 0 -or -not [StringComparer]::Ordinal.Equals($result[$result.Count - 1], $value)) {
            $result.Add($value)
        }
    }
    @($result)
}

function Assert-RegularDirectoryTree([string] $Root, [string] $Label) {
    $directories = @((Get-Item -LiteralPath $Root -Force)) + @(Get-ChildItem -LiteralPath $Root -Directory -Recurse -Force)
    foreach ($directory in $directories) {
        if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Label contains a reparse directory: $($directory.FullName)"
        }
    }
}
function Production-InputPaths {
    $sourceRoot = Join-Path $appRoot 'src'
    $staticRoot = Join-Path $appRoot 'static'
    Assert-RegularDirectoryTree $sourceRoot 'Renderer production source'
    Assert-RegularDirectoryTree $staticRoot 'Renderer static source'
    $paths = @(Get-ChildItem -LiteralPath $sourceRoot -File -Recurse -Force | ForEach-Object FullName | Where-Object {
        $relative = Relative-InputPath $_
        -not $relative.EndsWith('.test.ts',[StringComparison]::Ordinal) -and
        -not $relative.EndsWith('.test.svelte',[StringComparison]::Ordinal) -and
        $relative -cne 'nuclear-app/src/lib/AccessibleDialogHarness.svelte'
    })
    $paths += @(Get-ChildItem -LiteralPath $staticRoot -File -Recurse -Force | ForEach-Object FullName)
    $paths += @(@('package.json','package-lock.json','jsconfig.json','svelte.config.js','vite.config.js') | ForEach-Object { Join-Path $appRoot $_ })
    @(Sort-OrdinalUnique $paths)
}
function Harness-InputPaths([string] $SelectedSpec) {
    $supportRoot = Join-Path $appRoot 'e2e/browser/support'
    Assert-RegularDirectoryTree $supportRoot 'Renderer harness support'
    $paths = @(
        $PSCommandPath,
        (Join-Path $appRoot 'e2e/wdio.browser.conf.mjs'),
        (Join-Path $appRoot $SelectedSpec),
        (Join-Path $appRoot 'package.json'),
        (Join-Path $appRoot 'package-lock.json'),
        (Join-Path $appRoot 'jsconfig.json'),
        (Join-Path $appRoot 'svelte.config.js'),
        (Join-Path $appRoot 'vite.config.js')
    )
    $paths += @(Get-ChildItem -LiteralPath $supportRoot -File -Recurse -Force | ForEach-Object FullName)
    @(Sort-OrdinalUnique $paths)
}
function Input-Manifest([string] $Kind, [string[]] $Paths, [string] $ArchiveRoot) {
    $byPath = [Collections.Generic.SortedDictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($inputPath in $Paths) {
        $identity = Regular-FileIdentity $inputPath "$Kind input"
        $relative = Relative-InputPath $identity.path
        $archiveRelative = "inputs/$Kind/$relative"
        if ($ArchiveRoot) {
            $destination = Join-Path $ArchiveRoot $archiveRelative
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
            [IO.File]::WriteAllBytes($destination, [IO.File]::ReadAllBytes($identity.path))
            $archived = Regular-FileIdentity $destination "$Kind archived input"
            if ($archived.size -ne $identity.size -or $archived.sha256 -cne $identity.sha256) {
                throw "Archived $Kind input differs from source: $relative"
            }
        }
        $entry = [ordered]@{
            path = $relative
            archivePath = $archiveRelative
            size = $identity.size
            sha256 = $identity.sha256
        }
        if ($byPath.ContainsKey($relative)) { throw "Duplicate $Kind input path: $relative" }
        $byPath.Add($relative, $entry)
    }
    $files = @($byPath.Values)
    if ($files.Count -eq 0) { throw "$Kind input manifest is empty." }
    $bytes = [Text.Encoding]::UTF8.GetBytes(($files | ConvertTo-Json -Depth 4 -Compress))
    [ordered]@{
        schemaVersion = 'renderer-input-manifest/v1'
        kind = $Kind
        aggregateHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
        files = $files
    }
}
function Write-Utf8Json([string] $Path, [object] $Value) {
    [IO.File]::WriteAllText([IO.Path]::GetFullPath($Path), (($Value | ConvertTo-Json -Depth 10) + "`n"), [Text.UTF8Encoding]::new($false))
}
function Manifests-Match([object] $Before, [object] $After) {
    ($Before | ConvertTo-Json -Depth 8 -Compress) -ceq ($After | ConvertTo-Json -Depth 8 -Compress)
}
function Identities-Match([object] $Before, [object] $After) {
    $Before.path -ceq $After.path -and $Before.size -eq $After.size -and $Before.sha256 -ceq $After.sha256
}

foreach ($executable in @($ChromeBinary,$ChromeDriverBinary,$nodePath)) {
    if (-not [IO.Path]::IsPathFullyQualified($executable)) {
        throw "Renderer executable must be absolute: $executable"
    }
}
$nodeIdentity = Regular-FileIdentity $nodePath 'Renderer Node executable'
$browserIdentity = Regular-FileIdentity $ChromeBinary 'Renderer browser executable'
$driverIdentity = Regular-FileIdentity $ChromeDriverBinary 'Renderer driver executable'
$nodeVersion = & $nodePath --version
if ($LASTEXITCODE -ne 0 -or $nodeVersion -cne 'v22.23.1') {
    throw 'Renderer tests require Node 22.23.1.'
}
$browserVersion = (Get-Item -LiteralPath $ChromeBinary).VersionInfo.ProductVersion
if ($browserVersion -cne $ExpectedBrowserVersion) {
    throw 'Installed Chrome differs from the pinned renderer version.'
}
$driverVersion = & $ChromeDriverBinary --version
if ($LASTEXITCODE -ne 0 -or $driverVersion -cnotmatch "^ChromeDriver $([regex]::Escape($ExpectedBrowserVersion))(?: |$)") {
    throw 'ChromeDriver differs from the pinned renderer version.'
}

$runId = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path (Join-Path $repositoryRoot 'target/renderer-checks') "$Suite-$runId"
$profileRoot = Join-Path $runRoot 'profiles'
$profile = Join-Path $profileRoot "nuclear-renderer-$runId"
New-Item -ItemType Directory -Path $profile | Out-Null
Set-Content -LiteralPath (Join-Path $runRoot '.nuclear-renderer-check') -Value $runId -NoNewline
Write-Utf8Json (Join-Path $profileRoot '.nuclear-renderer-profile-root.json') ([ordered]@{
    schemaVersion = 'renderer-profile-owner/v1'
    runId = $runId
    profile = [IO.Path]::GetFileName($profile)
})
$windows = Get-ItemProperty -LiteralPath 'HKLM:/SOFTWARE/Microsoft/Windows NT/CurrentVersion'
$osVersion = "$($windows.CurrentBuildNumber).$($windows.UBR)"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RendererSystemDpi {
    [DllImport("user32.dll")] private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] private static extern uint GetDpiForSystem();
    public static uint Read() {
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero) throw new InvalidOperationException("Cannot establish DPI context.");
        try { return GetDpiForSystem(); } finally { SetThreadDpiAwarenessContext(previous); }
    }
}
'@
$systemScale = [int][Math]::Round([RendererSystemDpi]::Read() * 100.0 / 96.0)
$spec = switch ($Suite) {
    'workflows' { 'e2e/browser/renderer-workflows.e2e.mjs' }
    'performance' { 'e2e/browser/performance-acceptance.e2e.mjs' }
    'visual' { 'e2e/browser/visual-baseline.e2e.mjs' }
}
$productionManifest = Input-Manifest 'production' (Production-InputPaths) $runRoot
$harnessManifest = Input-Manifest 'harness' (Harness-InputPaths $spec) $runRoot
$productionManifestPath = Join-Path $runRoot 'production-manifest.json'
$harnessManifestPath = Join-Path $runRoot 'harness-manifest.json'
Write-Utf8Json $productionManifestPath $productionManifest
Write-Utf8Json $harnessManifestPath $harnessManifest
$productionManifestSha = (Get-FileHash -LiteralPath $productionManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$harnessManifestSha = (Get-FileHash -LiteralPath $harnessManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
$commit = git rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $commit -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Cannot identify renderer source commit.'
}

$command = @(
    'node_modules/@wdio/cli/bin/wdio.js',
    'run',
    './e2e/wdio.browser.conf.mjs',
    '--spec',
    "./$spec",
    '--outputDir',
    $runRoot
)
$originalEnvironment = @{}
$environment = [ordered]@{
    PATH = (Split-Path -Parent $nodePath) + [IO.Path]::PathSeparator + $env:PATH
    NUCLEAR_E2E_RUN_ID = $runId
    NUCLEAR_E2E_BROWSER_PROFILE_ROOT = $profileRoot
    NUCLEAR_E2E_BROWSER_PROFILE = $profile
    NUCLEAR_E2E_CHROME_BINARY = $ChromeBinary
    NUCLEAR_E2E_CHROMEDRIVER_BINARY = $ChromeDriverBinary
    NUCLEAR_E2E_CHROME_VERSION = $ExpectedBrowserVersion
    NUCLEAR_E2E_SCALE = ($ScalePercent / 100.0).ToString([Globalization.CultureInfo]::InvariantCulture)
    NUCLEAR_E2E_QUEUE_SIZE = [string] $QueueSize
    NUCLEAR_RENDERER_OUTPUT_DIRECTORY = $runRoot
    NUCLEAR_VISUAL_OUTPUT_DIRECTORY = $(if ($Suite -eq 'visual') { $runRoot } else { $null })
    NUCLEAR_VISUAL_SOURCE_COMMIT = $commit
    NUCLEAR_VISUAL_PRODUCTION_HASH = $productionManifest.aggregateHash
    NUCLEAR_VISUAL_OS_VERSION = $osVersion
    NUCLEAR_VISUAL_WINDOWS_SCALE_PERCENT = [string] $systemScale
}
$startedAt = [DateTime]::UtcNow.ToString('o')
$exitCode = $null
$failure = $null
$inputsVerifiedAfter = $false
Push-Location $repositoryRoot
try {
    foreach ($entry in $environment.GetEnumerator()) {
        $originalEnvironment[$entry.Key] = [Environment]::GetEnvironmentVariable($entry.Key, 'Process')
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    Set-Location -LiteralPath $appRoot
    & $nodePath @command *> (Join-Path $runRoot 'renderer.log')
    $exitCode = $LASTEXITCODE
} catch {
    $failure = $_.Exception.Message
} finally {
    Set-Location -LiteralPath $repositoryRoot
    try {
        $productionAfter = Input-Manifest 'production' (Production-InputPaths) $null
        $harnessAfter = Input-Manifest 'harness' (Harness-InputPaths $spec) $null
        if (-not (Manifests-Match $productionManifest $productionAfter) -or
            -not (Manifests-Match $harnessManifest $harnessAfter)) {
            throw 'Renderer input set or content changed during execution.'
        }
        if (-not (Identities-Match $nodeIdentity (Regular-FileIdentity $nodePath 'Renderer Node executable')) -or
            -not (Identities-Match $browserIdentity (Regular-FileIdentity $ChromeBinary 'Renderer browser executable')) -or
            -not (Identities-Match $driverIdentity (Regular-FileIdentity $ChromeDriverBinary 'Renderer driver executable'))) {
            throw 'A renderer executable changed during execution.'
        }
        $inputsVerifiedAfter = $true
    } catch {
        if (-not $failure) { $failure = $_.Exception.Message }
    }
    Pop-Location
    foreach ($entry in $originalEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    $receipt = [ordered]@{
        schemaVersion = 'renderer-check-receipt/v1'
        runId = $runId
        suite = $Suite
        selectedSpec = $spec
        source = [ordered]@{
            commit = $commit
            productionHash = $productionManifest.aggregateHash
            manifest = [ordered]@{ path='production-manifest.json'; sha256=$productionManifestSha }
        }
        harness = [ordered]@{
            hash = $harnessManifest.aggregateHash
            manifest = [ordered]@{ path='harness-manifest.json'; sha256=$harnessManifestSha }
        }
        inputsVerifiedAfter = $inputsVerifiedAfter
        startedAtUtc = $startedAt
        finishedAtUtc = [DateTime]::UtcNow.ToString('o')
        exitCode = $exitCode
        failure = $failure
        queueSize = $QueueSize
        scaleFactor = $ScalePercent / 100.0
        scalingMode = 'emulated'
        capture = [ordered]@{
            queueSize = $QueueSize
            scalePercent = $ScalePercent
            scaleFactor = $ScalePercent / 100.0
            scalingMode = 'emulated'
            command = @($nodePath) + $command
        }
        environment = [ordered]@{
            windowsBuild = $osVersion
            actualWindowsScalePercent = $systemScale
            profileRoot = $profileRoot
            profile = $profile
        }
        executables = [ordered]@{
            node = [ordered]@{ path=$nodeIdentity.path; size=$nodeIdentity.size; sha256=$nodeIdentity.sha256; version=$nodeVersion }
            browser = [ordered]@{ path=$browserIdentity.path; size=$browserIdentity.size; sha256=$browserIdentity.sha256; version=$browserVersion }
            driver = [ordered]@{ path=$driverIdentity.path; size=$driverIdentity.size; sha256=$driverIdentity.sha256; version=$driverVersion }
        }
    }
    Write-Utf8Json (Join-Path $runRoot 'run-receipt.json') $receipt
    Write-Output "RENDERER_EVIDENCE=$runRoot"
    if (Test-Path -LiteralPath (Join-Path $runRoot 'renderer.log')) {
        Get-Content -LiteralPath (Join-Path $runRoot 'renderer.log') -Tail 30
    }
}
if ($failure) { throw $failure }
if ($exitCode -ne 0) {
    throw "Renderer $Suite failed with exit code $exitCode; see retained evidence."
}
