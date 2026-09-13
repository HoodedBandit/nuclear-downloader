[CmdletBinding()]
param(
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Version = '0.7.1',

    [string] $BuildRoot,

    [switch] $PreflightOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
$script:TargetTriple = 'x86_64-pc-windows-msvc'
$script:RegisteredExecutable = 'nuclear-app\src-tauri\target\install-audit-v0.5.4\nuclear.exe'
. (Join-Path $PSScriptRoot 'local-packaged-build-contract.ps1')

function Get-CommandVersion {
    param([string] $Executable, [string[]] $Arguments)
    $output = @(& $Executable @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0 -or $output.Count -eq 0) {
        throw "Could not read the version from $Executable."
    }
    return ([string]$output[0]).Trim()
}

function Get-LowerSha256 {
    param([string] $Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-ToolIdentity {
    param([string] $Path, [string] $Version)
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Build tool must be a regular non-reparse file: $Path"
    }
    [ordered]@{
        path = $item.FullName
        size = [long]$item.Length
        sha256 = Get-LowerSha256 $item.FullName
        version = $Version
    }
}

function Assert-ToolIdentityUnchanged {
    param([object] $Before)
    $after = Get-ToolIdentity -Path ([string]$Before.path) -Version ([string]$Before.version)
    if ($after.size -ne $Before.size -or $after.sha256 -cne $Before.sha256) {
        throw "Build tool changed during construction: $($Before.path)"
    }
}

function Get-SourceIdentity {
    param([string] $RepositoryRoot)
    $commit = ([string](& git -C $RepositoryRoot rev-parse HEAD)).Trim()
    if ($LASTEXITCODE -ne 0 -or $commit -cnotmatch '^[0-9a-f]{40}$') {
        throw 'Could not determine the source revision.'
    }
    $status = @(& git -C $RepositoryRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the source worktree.' }
    $diff = @(& git -C $RepositoryRoot diff --binary HEAD -- .)
    if ($LASTEXITCODE -ne 0) { throw 'Could not calculate the tracked source diff.' }
    $untracked = @(& git -C $RepositoryRoot ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate untracked source files.' }
    $untrackedRecords = foreach ($relative in ($untracked | Sort-Object)) {
        $path = Join-Path $RepositoryRoot $relative
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            "$relative`0$((Get-Item -LiteralPath $path).Length)`0$(Get-LowerSha256 $path)"
        }
    }
    $material = "commit=$commit`nstatus:`n$($status -join "`n")`ndiff:`n$($diff -join "`n")`nuntracked:`n$($untrackedRecords -join "`n")"
    $bytes = $script:Utf8NoBom.GetBytes($material)
    $hash = [Security.Cryptography.SHA256]::HashData($bytes)
    return [ordered]@{
        commit = $commit
        dirty = ($status.Count -ne 0)
        digest = ([Convert]::ToHexString($hash)).ToLowerInvariant()
        status = @($status)
    }
}

function Assert-VersionParity {
    param([string] $RepositoryRoot, [string] $ExpectedVersion)
    $packageVersion = [string](Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\package.json') | ConvertFrom-Json).version
    $tauri = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\src-tauri\tauri.conf.json') | ConvertFrom-Json
    $cargoText = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot 'nuclear-app\src-tauri\Cargo.toml')
    $cargoMatch = [regex]::Match($cargoText, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
    if (-not $cargoMatch.Success -or $packageVersion -cne $ExpectedVersion -or
        [string]$tauri.version -cne $ExpectedVersion -or $cargoMatch.Groups[1].Value -cne $ExpectedVersion) {
        throw "Version mismatch: requested=$ExpectedVersion package=$packageVersion tauri=$($tauri.version) cargo=$($cargoMatch.Groups[1].Value)"
    }
    Assert-NuclearPackagedConfiguration -Configuration $tauri -ExpectedVersion $ExpectedVersion
}

function Get-ArtifactRecord {
    param([string] $Path, [string] $OwnedRoot, [string] $Kind)
    $full = Get-NuclearContainedPath -Candidate $Path -Root $OwnedRoot
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { throw "Missing packaged artifact: $full" }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.Length -le 0) {
        throw "Packaged artifact is empty or unsafe: $full"
    }
    return [ordered]@{
        kind = $Kind
        path = [IO.Path]::GetRelativePath($OwnedRoot, $full).Replace('\', '/')
        size = [long]$item.Length
        sha256 = Get-LowerSha256 $full
    }
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Assert-NuclearExactGitRoot -RepositoryRoot $repositoryRoot
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$tauriRoot = Join-Path $appRoot 'src-tauri'
$targetRoot = Join-Path $tauriRoot 'target'
$registeredPath = Join-Path $repositoryRoot $script:RegisteredExecutable
if (-not $BuildRoot) { $BuildRoot = Join-Path $targetRoot 'local-packaged-builds' }
$buildRootFull = Get-NuclearContainedPath -Candidate $BuildRoot -Root $targetRoot
Assert-NuclearNoReparseComponents -Candidate $buildRootFull -Root $targetRoot | Out-Null
if (Test-NuclearPathOverlap -First $buildRootFull -Second $registeredPath) {
    throw "Build root overlaps the registered active installation and is forbidden: $registeredPath"
}

Assert-VersionParity -RepositoryRoot $repositoryRoot -ExpectedVersion $Version
$updateKeysBefore = Get-NuclearUpdateKeyConfiguration `
    -CurrentId ([string]$env:NUCLEAR_UPDATE_KEY_ID) `
    -CurrentPublicKey ([string]$env:NUCLEAR_UPDATE_PUBLIC_KEY) `
    -NextId ([string]$env:NUCLEAR_UPDATE_NEXT_KEY_ID) `
    -NextPublicKey ([string]$env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY)
$nodeExecutable = Join-Path $targetRoot 'toolchains\node-v22.23.1-win-x64\node.exe'
$npmExecutable = Join-Path $targetRoot 'toolchains\npm-10.9.9\npm.cmd'
if (-not (Test-Path -LiteralPath $nodeExecutable -PathType Leaf)) {
    $nodeExecutable = (Get-Command node.exe -ErrorAction Stop).Source
}
if (-not (Test-Path -LiteralPath $npmExecutable -PathType Leaf)) {
    $npmExecutable = (Get-Command npm.cmd -ErrorAction Stop).Source
}
$tauriCli = Join-Path $appRoot 'node_modules\@tauri-apps\cli\tauri.js'
$nodeVersion = Get-CommandVersion $nodeExecutable @('--version')
$npmVersion = Get-CommandVersion $npmExecutable @('--version')
$tauriVersion = Get-CommandVersion $nodeExecutable @($tauriCli, '--version')
$rustcVersion = Get-CommandVersion rustc.exe @('--version')
$cargoVersion = Get-CommandVersion cargo.exe @('--version')
if ($nodeVersion -cne 'v22.23.1' -or $npmVersion -cne '10.9.9' -or
    $rustcVersion -cnotmatch '^rustc 1\.94\.1 ' -or $cargoVersion -cnotmatch '^cargo 1\.94\.1 ') {
    throw "Pinned toolchains required: node v22.23.1, npm 10.9.9, rustc/cargo 1.94.1. Found: $nodeVersion; npm $npmVersion; $rustcVersion; $cargoVersion"
}
$rustcExecutable = (Get-Command rustc.exe -ErrorAction Stop).Source
$cargoExecutable = (Get-Command cargo.exe -ErrorAction Stop).Source
$toolchains = [ordered]@{
    node = Get-ToolIdentity $nodeExecutable $nodeVersion
    npm = Get-ToolIdentity $npmExecutable $npmVersion
    tauri = Get-ToolIdentity $tauriCli $tauriVersion
    rustc = Get-ToolIdentity $rustcExecutable $rustcVersion
    cargo = Get-ToolIdentity $cargoExecutable $cargoVersion
}

$lockPath = Join-Path $tauriRoot 'sidecars.lock.json'
$sidecarLock = Get-Content -Raw -LiteralPath $lockPath | ConvertFrom-Json
foreach ($sidecar in $sidecarLock.sidecars) {
    $sidecarPath = Join-Path (Join-Path $tauriRoot 'binaries') ([string]$sidecar.filename)
    if (-not (Test-Path -LiteralPath $sidecarPath -PathType Leaf) -or
        (Get-LowerSha256 $sidecarPath) -cne [string]$sidecar.sha256) {
        throw "Pinned sidecar payload is missing or mismatched: $($sidecar.filename)"
    }
}

$sourceBefore = Get-SourceIdentity $repositoryRoot
if ($PreflightOnly) {
    [pscustomobject]@{ version = $Version; buildRoot = $buildRootFull; source = $sourceBefore; toolchains = $toolchains; updateKeys = $updateKeysBefore }
    return
}

$runId = "local-$($Version)-$([DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ'))-$([Guid]::NewGuid().ToString('N'))"
$ownedRoot = Get-NuclearContainedPath -Candidate (Join-Path $buildRootFull $runId) -Root $buildRootFull
if (Test-Path -LiteralPath $ownedRoot) { throw "Refusing stale or reused build output: $ownedRoot" }
$cargoTarget = Join-Path $ownedRoot 'cargo-target'
$receiptPath = Join-Path $ownedRoot 'local-packaged-build-receipt.json'
if (-not (Test-Path -LiteralPath $buildRootFull)) {
    New-Item -ItemType Directory -Path $buildRootFull | Out-Null
    Assert-NuclearNoReparseComponents -Candidate $buildRootFull -Root $targetRoot | Out-Null
}
New-Item -ItemType Directory -Path $ownedRoot | Out-Null
Assert-NuclearNoReparseComponents -Candidate $ownedRoot -Root $targetRoot | Out-Null
New-Item -ItemType Directory -Path $cargoTarget | Out-Null
Assert-NuclearNoReparseComponents -Candidate $cargoTarget -Root $targetRoot | Out-Null
[IO.File]::WriteAllText((Join-Path $ownedRoot '.nuclear-local-build-owned'), $runId, $script:Utf8NoBom)

$startedAt = [DateTimeOffset]::UtcNow
$command = @($nodeExecutable,$tauriCli,'build','--no-sign','--bundles','nsis','--target',$script:TargetTriple)
$previousTarget = $env:CARGO_TARGET_DIR
$previousPath = $env:PATH
try {
    $env:CARGO_TARGET_DIR = $cargoTarget
    $env:PATH = "$(Split-Path -Parent $npmExecutable);$(Split-Path -Parent $nodeExecutable);$previousPath"
    Push-Location $appRoot
    try { & $nodeExecutable $tauriCli build --no-sign --bundles nsis --target $script:TargetTriple }
    finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw 'The Tauri packaged build failed.' }
} finally {
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:PATH = $previousPath
}

if (Test-Path -LiteralPath $receiptPath) { throw "Refusing a stale same-version receipt: $receiptPath" }
$releaseRoot = Join-Path (Join-Path $cargoTarget $script:TargetTriple) 'release'
$executablePath = Join-Path $releaseRoot 'nuclear.exe'
$fileVersion = (Get-Item -LiteralPath $executablePath).VersionInfo.ProductVersion
Assert-NuclearExactExecutableVersion -ActualVersion $fileVersion -ExpectedVersion $Version
$artifacts = @()
$artifacts += Get-ArtifactRecord $executablePath $ownedRoot 'standalone-packaged-executable'
foreach ($name in @('yt-dlp.exe','ffmpeg.exe','ffprobe.exe','deno.exe')) {
    $builtSidecar = Join-Path $releaseRoot $name
    $lockEntry = @($sidecarLock.sidecars | Where-Object { "$($_.name).exe" -ceq $name })
    if ($lockEntry.Count -ne 1 -or (Get-LowerSha256 $builtSidecar) -cne [string]$lockEntry[0].sha256) {
        throw "Packaged sidecar payload does not match sidecars.lock.json: $name"
    }
    $artifacts += Get-ArtifactRecord $builtSidecar $ownedRoot 'sidecar'
}
$installerCandidates = @(Get-ChildItem -LiteralPath (Join-Path $releaseRoot 'bundle\nsis') -File |
    Where-Object { $_.Name -ceq "Nuclear Downloader_${Version}_x64-setup.exe" })
if ($installerCandidates.Count -ne 1) { throw "Expected one exact NSIS installer; found $($installerCandidates.Count)." }
$artifacts += Get-ArtifactRecord $installerCandidates[0].FullName $ownedRoot 'nsis-installer-container'

$sourceAfter = Get-SourceIdentity $repositoryRoot
if ($sourceAfter.commit -cne $sourceBefore.commit -or $sourceAfter.digest -cne $sourceBefore.digest) {
    throw 'Source changed during the build; no success receipt was written.'
}
foreach ($tool in $toolchains.Values) { Assert-ToolIdentityUnchanged $tool }
$updateKeysAfter = Get-NuclearUpdateKeyConfiguration `
    -CurrentId ([string]$env:NUCLEAR_UPDATE_KEY_ID) `
    -CurrentPublicKey ([string]$env:NUCLEAR_UPDATE_PUBLIC_KEY) `
    -NextId ([string]$env:NUCLEAR_UPDATE_NEXT_KEY_ID) `
    -NextPublicKey ([string]$env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY)
if (($updateKeysAfter | ConvertTo-Json -Compress -Depth 5) -cne ($updateKeysBefore | ConvertTo-Json -Compress -Depth 5)) {
    throw 'Updater public-key configuration changed during the build; no success receipt was written.'
}
$receipt = [ordered]@{
    schemaVersion = 'nuclear-local-packaged-build/v1'
    runId = $runId
    purpose = 'local-packaged-custom-protocol-gui'
    version = $Version
    target = $script:TargetTriple
    startedAtUtc = $startedAt.ToString('o')
    finishedAtUtc = [DateTimeOffset]::UtcNow.ToString('o')
    source = $sourceAfter
    command = $command
    environment = [ordered]@{ cargoTargetDir = 'cargo-target'; buildMode = 'tauri-build'; devServerUsed = $false; frontendDist = '../build' }
    toolchains = $toolchains
    inputs = [ordered]@{
        sidecarsLock = [ordered]@{ path='nuclear-app/src-tauri/sidecars.lock.json'; sha256=(Get-LowerSha256 $lockPath) }
        updateKeys = $updateKeysAfter
    }
    artifacts = @($artifacts)
    qualification = [ordered]@{
        standaloneExecutableBuilt = $true
        installerContainerBuilt = $true
        installerEmbeddedExecutableExtracted = $false
        installerEmbeddedExecutableVerified = $false
        toolBytesVerifiedAfter = $true
        dependencyTreeByteIdentityRecorded = $false
        reproducibleBuildProven = $false
        launched = $false
        controlsTested = $false
        workflowsTested = $false
    }
}
$temporaryReceipt = Join-Path $ownedRoot ".receipt-$([Guid]::NewGuid().ToString('N')).tmp"
[IO.File]::WriteAllText($temporaryReceipt, ($receipt | ConvertTo-Json -Depth 10), $script:Utf8NoBom)
Move-Item -LiteralPath $temporaryReceipt -Destination $receiptPath
Write-Output $receiptPath
