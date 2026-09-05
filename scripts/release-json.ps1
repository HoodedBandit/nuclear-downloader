function Read-BoundedReleaseJson {
    param([Parameter(Mandatory)] [string] $Path, [Parameter(Mandatory)] [long] $Limit)

    # Release contracts must preserve JSON strings exactly. PowerShell's default
    # date coercion otherwise changes ISO timestamps when evidence is serialized.
    if (-not (Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) {
        throw 'Release evidence requires PowerShell 7.5 or newer to preserve JSON timestamp strings.'
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0 -or $item.Length -gt $Limit) {
        throw "Release JSON is not a bounded regular file: $Path"
    }
    $bytes = [System.IO.File]::ReadAllBytes($item.FullName)
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf) {
        throw 'Release JSON must use UTF-8 without a byte-order mark.'
    }
    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    return ConvertFrom-Json -InputObject $utf8.GetString($bytes) -DateKind String
}
