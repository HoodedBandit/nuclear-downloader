$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-WebView2RuntimeVersion {
    $registryPaths = @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    )
    foreach ($registryPath in $registryPaths) {
        $value = Get-ItemPropertyValue -LiteralPath $registryPath -Name 'pv' -ErrorAction SilentlyContinue
        if ([string]$value -match '^[1-9][0-9]*\.[0-9]+\.[0-9]+\.[0-9]+$') {
            return [string]$value
        }
    }
    throw 'Candidate acceptance could not determine the installed WebView2 runtime version.'
}

function Convert-VersionResponseToText {
    param([Parameter(Mandatory)] $Content)

    if ($Content -is [byte[]]) {
        $bytes = [byte[]]$Content
        if ($bytes.Length -gt 1024) {
            throw 'The EdgeDriver version response exceeded 1 KiB.'
        }
        if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
            return [System.Text.Encoding]::Unicode.GetString($bytes, 2, $bytes.Length - 2).Trim()
        }
        if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
            return [System.Text.UTF8Encoding]::new($false, $true).GetString($bytes, 3, $bytes.Length - 3).Trim()
        }
        return [System.Text.UTF8Encoding]::new($false, $true).GetString($bytes).Trim()
    }
    $text = [string]$Content
    if ($text.Length -gt 1024) {
        throw 'The EdgeDriver version response exceeded 1 KiB.'
    }
    return $text.Trim()
}

function Install-CompatibleEdgeDriver {
    param(
        [Parameter(Mandatory)] [string] $DestinationDirectory,
        [Parameter(Mandatory)] [string] $RuntimeVersion
    )

    $runtimeParts = $RuntimeVersion.Split('.')
    if ($runtimeParts.Count -ne 4) {
        throw "The WebView2 runtime version is malformed: $RuntimeVersion"
    }
    $runtimeBuild = $runtimeParts[0..2] -join '.'
    $majorVersion = $runtimeParts[0]
    $versionUri = "https://msedgedriver.microsoft.com/LATEST_RELEASE_$majorVersion"
    $versionResponse = Invoke-WebRequest -Uri $versionUri -UseBasicParsing -TimeoutSec 15
    $driverVersion = Convert-VersionResponseToText -Content $versionResponse.Content
    if ($driverVersion -notmatch '^[1-9][0-9]*\.[0-9]+\.[0-9]+\.[0-9]+$') {
        throw "Microsoft returned a malformed EdgeDriver version: $driverVersion"
    }
    $driverParts = $driverVersion.Split('.')
    if (($driverParts[0..2] -join '.') -cne $runtimeBuild) {
        throw "EdgeDriver $driverVersion does not match WebView2 runtime build $runtimeBuild."
    }

    New-Item -ItemType Directory -Path $DestinationDirectory | Out-Null
    $archivePath = Join-Path $DestinationDirectory 'edgedriver_win64.zip'
    $archiveUri = "https://msedgedriver.microsoft.com/$driverVersion/edgedriver_win64.zip"
    Invoke-WebRequest -Uri $archiveUri -OutFile $archivePath -UseBasicParsing -TimeoutSec 60
    $archiveItem = Get-Item -LiteralPath $archivePath -Force
    if ($archiveItem.PSIsContainer -or
        ($archiveItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $archiveItem.Length -le 0 -or $archiveItem.Length -gt 64MB) {
        throw 'The downloaded EdgeDriver archive is empty, oversized, or not a regular file.'
    }

    Add-Type -AssemblyName System.IO.Compression
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $matches = @($archive.Entries | Where-Object { $_.FullName -ceq 'msedgedriver.exe' })
        if ($matches.Count -ne 1) {
            throw 'The EdgeDriver archive must contain exactly one root msedgedriver.exe.'
        }
        $entry = $matches[0]
        if ($entry.Length -le 0 -or $entry.Length -gt 64MB) {
            throw 'The EdgeDriver executable is empty or exceeds 64 MiB.'
        }
        $driverPath = Join-Path $DestinationDirectory 'msedgedriver.exe'
        $source = $entry.Open()
        $destination = [System.IO.File]::Open(
            $driverPath,
            [System.IO.FileMode]::CreateNew,
            [System.IO.FileAccess]::Write,
            [System.IO.FileShare]::None
        )
        try {
            $source.CopyTo($destination)
        } finally {
            $destination.Dispose()
            $source.Dispose()
        }
    } finally {
        $archive.Dispose()
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $driverPath
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
        -not $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch '(^|, )O=Microsoft Corporation(,|$)') {
        throw 'The downloaded EdgeDriver executable does not have a valid Microsoft Authenticode signature.'
    }
    $versionOutput = & $driverPath '--version'
    if ($LASTEXITCODE -ne 0 -or [string]$versionOutput -notmatch "^Microsoft Edge WebDriver $([regex]::Escape($driverVersion)) ") {
        throw "The downloaded EdgeDriver executable did not report version $driverVersion."
    }
    Write-Host "Using Microsoft EdgeDriver $driverVersion for WebView2 runtime $RuntimeVersion."
    return [pscustomobject]@{
        Path = [System.IO.Path]::GetFullPath($driverPath)
        Version = $driverVersion
    }
}
