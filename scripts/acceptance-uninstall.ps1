Set-StrictMode -Version Latest

function Get-AcceptanceFileFingerprint {
    param([Parameter(Mandatory)] [string] $Path)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0) {
        throw "Acceptance retained data is not a non-empty regular file: $Path"
    }
    return [pscustomobject]@{
        Size = [long]$item.Length
        Sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Wait-AcceptanceInstallRootRemoved {
    param(
        [Parameter(Mandatory)] [string] $InstallRoot,
        [ValidateRange(1, 300000)] [int] $TimeoutMilliseconds = 30000,
        [ValidateRange(1, 1000)] [int] $PollMilliseconds = 100
    )

    $deadline = [DateTimeOffset]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    while ((Test-Path -LiteralPath $InstallRoot) -and [DateTimeOffset]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds $PollMilliseconds
    }
    if (Test-Path -LiteralPath $InstallRoot) {
        throw "NSIS uninstall left the owned installation root behind: $InstallRoot"
    }
}

function Assert-AcceptanceFileUnchanged {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [object] $Before
    )

    $after = Get-AcceptanceFileFingerprint -Path $Path
    if ($after.Size -ne [long]$Before.Size -or $after.Sha256 -cne [string]$Before.Sha256) {
        throw "NSIS uninstall changed retained per-user data: $Path"
    }
}
