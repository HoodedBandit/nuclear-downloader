[CmdletBinding()]
param(
    [string]$LockFile = (Join-Path $PSScriptRoot '..\nuclear-app\src-tauri\sidecars.lock.json'),
    [string]$Destination = (Join-Path $PSScriptRoot '..\nuclear-app\src-tauri\binaries')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$maxDownloadBytes = 1GB
$maxArchiveEntries = 256
$maxExpandedBytes = 2GB
$maxEntryBytes = 1GB
$maxCompressionRatio = 200.0

function Get-CanonicalPath {
    param([Parameter(Mandatory)][string]$Path)

    return [System.IO.Path]::GetFullPath($Path).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
}

function Assert-PathContained {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Candidate
    )

    $rootPath = Get-CanonicalPath $Root
    $candidatePath = [System.IO.Path]::GetFullPath($Candidate)
    $relative = [System.IO.Path]::GetRelativePath($rootPath, $candidatePath)
    if (
        [System.IO.Path]::IsPathRooted($relative) -or
        $relative -eq '..' -or
        $relative.StartsWith("..$([System.IO.Path]::DirectorySeparatorChar)", [System.StringComparison]::Ordinal) -or
        $relative.StartsWith("..$([System.IO.Path]::AltDirectorySeparatorChar)", [System.StringComparison]::Ordinal)
    ) {
        throw "Path escapes the expected root: $candidatePath"
    }
}

function Assert-NonReparseDirectory {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Label
    )

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer) {
        throw "$Label must be a directory: $Path"
    }
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must not be a symbolic link or reparse point: $Path"
    }
}

function Invoke-BoundedDownload {
    param(
        [Parameter(Mandatory)][System.Net.Http.HttpClient]$Client,
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$OutputPath
    )

    $sourceUri = [System.Uri]$Url
    if (
        $sourceUri.Scheme -cne 'https' -or
        -not [string]::IsNullOrEmpty($sourceUri.UserInfo)
    ) {
        throw "Sidecar source must use credential-free HTTPS: $Url"
    }

    $response = $Client.GetAsync(
        $Url,
        [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
    ).GetAwaiter().GetResult()
    try {
        $response.EnsureSuccessStatusCode() | Out-Null
        $finalUri = $response.RequestMessage.RequestUri
        if (
            $null -eq $finalUri -or
            $finalUri.Scheme -cne 'https' -or
            -not [string]::IsNullOrEmpty($finalUri.UserInfo)
        ) {
            throw "Sidecar download redirected outside credential-free HTTPS: $Url"
        }
        $declaredLength = $response.Content.Headers.ContentLength
        if ($null -ne $declaredLength -and $declaredLength -gt $maxDownloadBytes) {
            throw "Sidecar source exceeds the $maxDownloadBytes-byte download limit: $Url"
        }

        $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        if ($input.CanTimeout) {
            $input.ReadTimeout = 30000
        }
        $output = [System.IO.File]::Open(
            $OutputPath,
            [System.IO.FileMode]::CreateNew,
            [System.IO.FileAccess]::Write,
            [System.IO.FileShare]::None
        )
        try {
            $buffer = [byte[]]::new(1MB)
            [long]$total = 0
            while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $total += $read
                if ($total -gt $maxDownloadBytes) {
                    throw "Sidecar source exceeded the $maxDownloadBytes-byte download limit: $Url"
                }
                $output.Write($buffer, 0, $read)
            }
            $output.Flush($true)
        }
        finally {
            $output.Dispose()
            $input.Dispose()
        }
    }
    finally {
        $response.Dispose()
    }
}

function Expand-LockedMember {
    param(
        [Parameter(Mandatory)][string]$ArchivePath,
        [Parameter(Mandatory)][string]$MemberSuffix,
        [Parameter(Mandatory)][string]$OutputPath
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        if ($archive.Entries.Count -gt $maxArchiveEntries) {
            throw "Sidecar archive contains too many entries."
        }

        [long]$expandedTotal = 0
        $matches = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $archive.Entries) {
            $normalized = $entry.FullName.Replace('\', '/')
            if (
                [System.IO.Path]::IsPathRooted($normalized) -or
                ($normalized.Split('/') | Where-Object { $_ -eq '..' }).Count -gt 0
            ) {
                throw "Sidecar archive contains an unsafe path: $($entry.FullName)"
            }
            if ($entry.Length -gt $maxEntryBytes) {
                throw "Sidecar archive entry exceeds the per-entry limit: $($entry.FullName)"
            }
            $expandedTotal += $entry.Length
            if ($expandedTotal -gt $maxExpandedBytes) {
                throw "Sidecar archive exceeds the expanded-size limit."
            }
            if (
                $entry.Length -gt 0 -and
                (
                    $entry.CompressedLength -eq 0 -or
                    ($entry.Length / [double]$entry.CompressedLength) -gt $maxCompressionRatio
                )
            ) {
                throw "Sidecar archive entry exceeds the compression-ratio limit: $($entry.FullName)"
            }
            if ($normalized.EndsWith($MemberSuffix, [System.StringComparison]::OrdinalIgnoreCase)) {
                $matches.Add($entry)
            }
        }

        if ($matches.Count -ne 1) {
            throw "Expected exactly one archive member ending in '$MemberSuffix'; found $($matches.Count)."
        }

        $input = $matches[0].Open()
        $output = [System.IO.File]::Open(
            $OutputPath,
            [System.IO.FileMode]::CreateNew,
            [System.IO.FileAccess]::Write,
            [System.IO.FileShare]::None
        )
        try {
            $buffer = [byte[]]::new(1MB)
            [long]$total = 0
            while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $total += $read
                if ($total -gt $maxEntryBytes) {
                    throw "Extracted sidecar exceeded the per-entry limit."
                }
                $output.Write($buffer, 0, $read)
            }
            $output.Flush($true)
        }
        finally {
            $output.Dispose()
            $input.Dispose()
        }
    }
    finally {
        $archive.Dispose()
    }
}

$lockPath = [System.IO.Path]::GetFullPath($LockFile)
if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
    throw "Sidecar lock manifest not found: $lockPath"
}
$lock = Get-Content -LiteralPath $lockPath -Raw | ConvertFrom-Json
if ($lock.schemaVersion -ne 1 -or $lock.platform -ne 'windows-x86_64') {
    throw 'Unsupported sidecar lock manifest schema or platform.'
}
$isWindowsPlatform = [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT
if (-not [Environment]::Is64BitOperatingSystem -or -not $isWindowsPlatform) {
    throw 'Sidecar acquisition supports only Windows x64.'
}

$destinationPath = [System.IO.Path]::GetFullPath($Destination)
New-Item -ItemType Directory -Path $destinationPath -Force | Out-Null
Assert-NonReparseDirectory -Path $destinationPath -Label 'Sidecar destination'
$temporaryBase = Get-CanonicalPath ([System.IO.Path]::GetTempPath())
$temporaryRoot = Join-Path $temporaryBase ("nuclear-sidecars-" + [guid]::NewGuid().ToString('N'))
Assert-PathContained -Root $temporaryBase -Candidate $temporaryRoot
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
$temporaryMarker = Join-Path $temporaryRoot '.nuclear-sidecars-owned'
[System.IO.File]::WriteAllText($temporaryMarker, "schemaVersion=1`n", [System.Text.UTF8Encoding]::new($false))

$handler = [System.Net.Http.HttpClientHandler]::new()
$handler.AllowAutoRedirect = $true
$client = [System.Net.Http.HttpClient]::new($handler)
$client.Timeout = [TimeSpan]::FromMinutes(30)
$downloads = @{}

try {
    foreach ($entry in $lock.sidecars) {
        if ($entry.architecture -ne 'x86_64-pc-windows-msvc') {
            throw "Unsupported architecture for $($entry.name): $($entry.architecture)"
        }
        if (-not ([string]$entry.sourceUrl).StartsWith('https://', [System.StringComparison]::Ordinal)) {
            throw "Sidecar source must use HTTPS: $($entry.name)"
        }
        if ([string]$entry.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw "Invalid locked SHA-256 for $($entry.name)"
        }

        $targetPath = Join-Path $destinationPath ([string]$entry.filename)
        Assert-PathContained -Root $destinationPath -Candidate $targetPath

        $sourceKey = [string]$entry.sourceUrl
        if (-not $downloads.ContainsKey($sourceKey)) {
            $downloadPath = Join-Path $temporaryRoot (([guid]::NewGuid().ToString('N')) + '.download')
            Invoke-BoundedDownload -Client $client -Url $sourceKey -OutputPath $downloadPath
            $downloads[$sourceKey] = $downloadPath
        }

        $candidatePath = Join-Path $temporaryRoot (([guid]::NewGuid().ToString('N')) + '.exe')
        $archiveMemberProperty = $entry.PSObject.Properties['archiveMemberSuffix']
        if ($null -ne $archiveMemberProperty -and [string]$archiveMemberProperty.Value -ne '') {
            Expand-LockedMember `
                -ArchivePath $downloads[$sourceKey] `
                -MemberSuffix ([string]$archiveMemberProperty.Value).Replace('\', '/') `
                -OutputPath $candidatePath
        }
        else {
            [System.IO.File]::Copy($downloads[$sourceKey], $candidatePath, $false)
        }

        $actualHash = (Get-FileHash -LiteralPath $candidatePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actualHash -ne ([string]$entry.sha256).ToLowerInvariant()) {
            throw "Hash mismatch for $($entry.name): expected $($entry.sha256), got $actualHash"
        }

        $adjacentPartial = "$targetPath.partial-$([guid]::NewGuid().ToString('N'))"
        Assert-PathContained -Root $destinationPath -Candidate $adjacentPartial
        try {
            [System.IO.File]::Copy($candidatePath, $adjacentPartial, $false)
            Move-Item -LiteralPath $adjacentPartial -Destination $targetPath -Force
        }
        finally {
            if (Test-Path -LiteralPath $adjacentPartial) {
                Remove-Item -LiteralPath $adjacentPartial -Force -ErrorAction SilentlyContinue
            }
        }
        Write-Host "Verified $($entry.name) $($entry.version) -> $targetPath"
    }
}
finally {
    $client.Dispose()
    $handler.Dispose()
    $resolvedTemporaryRoot = Get-CanonicalPath $temporaryRoot
    Assert-PathContained -Root $temporaryBase -Candidate $resolvedTemporaryRoot
    $markerIsValid = (
        (Test-Path -LiteralPath $temporaryMarker -PathType Leaf) -and
        ([System.IO.File]::ReadAllText($temporaryMarker) -ceq "schemaVersion=1`n")
    )
    $temporaryItem = Get-Item -LiteralPath $resolvedTemporaryRoot -Force -ErrorAction SilentlyContinue
    $isRegularDirectory = (
        $null -ne $temporaryItem -and
        $temporaryItem.PSIsContainer -and
        ($temporaryItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0
    )
    if (
        $markerIsValid -and
        $isRegularDirectory -and
        (Split-Path -Leaf $resolvedTemporaryRoot).StartsWith('nuclear-sidecars-', [System.StringComparison]::Ordinal)
    ) {
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    elseif (Test-Path -LiteralPath $resolvedTemporaryRoot) {
        Write-Warning "Refused to recursively remove an unverified sidecar staging path: $resolvedTemporaryRoot"
    }
}
