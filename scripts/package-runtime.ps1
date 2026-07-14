[CmdletBinding()]
param(
    [string]$RuntimeVersion,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$tauriRoot = Join-Path $repositoryRoot 'nuclear-app\src-tauri'
$binaryRoot = Join-Path $tauriRoot 'binaries'
$targetRoot = Join-Path $tauriRoot 'target'

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $targetRoot 'release-artifacts'
}

$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$stagingRoot = Join-Path $targetRoot 'runtime-bundle-staging'
foreach ($path in @($outputRoot, $stagingRoot)) {
    if (-not $path.StartsWith($targetRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Release path must stay inside $targetRoot"
    }
}

$tools = @(
    [pscustomobject]@{ Name = 'yt-dlp'; Source = 'yt-dlp-x86_64-pc-windows-msvc.exe'; Target = 'yt-dlp.exe'; VersionArgs = @('--version') },
    [pscustomobject]@{ Name = 'ffmpeg'; Source = 'ffmpeg-x86_64-pc-windows-msvc.exe'; Target = 'ffmpeg.exe'; VersionArgs = @('-version') },
    [pscustomobject]@{ Name = 'ffprobe'; Source = 'ffprobe-x86_64-pc-windows-msvc.exe'; Target = 'ffprobe.exe'; VersionArgs = @('-version') },
    [pscustomobject]@{ Name = 'deno'; Source = 'deno-x86_64-pc-windows-msvc.exe'; Target = 'deno.exe'; VersionArgs = @('--version') }
)

foreach ($tool in $tools) {
    $tool | Add-Member -NotePropertyName SourcePath -NotePropertyValue (Join-Path $binaryRoot $tool.Source)
    if (-not (Test-Path -LiteralPath $tool.SourcePath -PathType Leaf)) {
        throw "Missing runtime binary: $($tool.SourcePath)"
    }
}

if (-not $RuntimeVersion) {
    $RuntimeVersion = (& $tools[0].SourcePath --version | Select-Object -First 1).Trim()
}
if ($RuntimeVersion -notmatch '^[A-Za-z0-9._-]+$') {
    throw "Unsafe runtime version: $RuntimeVersion"
}

if (Test-Path -LiteralPath $stagingRoot) {
    Remove-Item -LiteralPath $stagingRoot -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stagingRoot, $outputRoot | Out-Null

$manifestTools = foreach ($tool in $tools) {
    $targetPath = Join-Path $stagingRoot $tool.Target
    Copy-Item -LiteralPath $tool.SourcePath -Destination $targetPath -Force
    $version = (& $tool.SourcePath @($tool.VersionArgs) | Select-Object -First 1).Trim()
    if (-not $version) {
        throw "Could not probe $($tool.Name) version."
    }
    [ordered]@{
        name = $tool.Name
        version = $version
        path = $tool.Target
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $targetPath).Hash.ToLowerInvariant()
    }
}

$manifest = [ordered]@{
    runtimeVersion = $RuntimeVersion
    platform = 'windows-x64'
    tools = @($manifestTools)
}
$manifestPath = Join-Path $stagingRoot 'runtime-manifest.json'
$manifestJson = $manifest | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText(
    $manifestPath,
    $manifestJson,
    [System.Text.UTF8Encoding]::new($false)
)

$archiveName = "nuclear-downloader-runtime-$RuntimeVersion-windows-x64.zip"
$archivePath = Join-Path $outputRoot $archiveName
$checksumPath = "$archivePath.sha256"
Remove-Item -LiteralPath $archivePath, $checksumPath -Force -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stagingRoot '*') -DestinationPath $archivePath -CompressionLevel Optimal
$archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
"$archiveHash  $archiveName" | Set-Content -LiteralPath $checksumPath -Encoding ascii

Remove-Item -LiteralPath $stagingRoot -Recurse -Force
Write-Output $archivePath
Write-Output $checksumPath
