$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Test-ByteSequence {
    param(
        [Parameter(Mandatory)] [byte[]] $Content,
        [Parameter(Mandatory)] [byte[]] $Sequence
    )

    if ($Sequence.Length -eq 0 -or $Sequence.Length -gt $Content.Length) {
        return $false
    }
    for ($offset = 0; $offset -le $Content.Length - $Sequence.Length; $offset++) {
        $matches = $true
        for ($index = 0; $index -lt $Sequence.Length; $index++) {
            if ($Content[$offset + $index] -ne $Sequence[$index]) {
                $matches = $false
                break
            }
        }
        if ($matches) { return $true }
    }
    return $false
}

function Protect-AcceptanceLog {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $ProtectedValues,
        [long] $MaximumBytes = 4MB
    )

    if ($MaximumBytes -lt 1 -or $MaximumBytes -gt 4MB) {
        throw 'Acceptance log retention limit must be between 1 byte and 4 MiB.'
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Acceptance logs must be regular non-reparse files: $Path"
    }

    $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
    $protected = @($ProtectedValues | Where-Object { -not [string]::IsNullOrEmpty($_) })
    $maximumProtectedBytes = 0
    foreach ($value in $protected) {
        $length = $utf8.GetByteCount($value)
        if ($length -gt 4096) {
            throw 'A protected acceptance value exceeds the 4096-byte fixture URL limit.'
        }
        $maximumProtectedBytes = [Math]::Max($maximumProtectedBytes, $length)
    }

    # Include enough overlap to contain any protected value that crosses the
    # retained-tail boundary. Redaction happens before the final size limit.
    $readBytes = [Math]::Min($item.Length, $MaximumBytes + $maximumProtectedBytes)
    $buffer = [byte[]]::new([int]$readBytes)
    $source = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        if ($item.Length -gt $readBytes) {
            [void]$source.Seek(-$readBytes, [System.IO.SeekOrigin]::End)
        }
        $offset = 0
        while ($offset -lt $buffer.Length) {
            $count = $source.Read($buffer, $offset, $buffer.Length - $offset)
            if ($count -eq 0) { break }
            $offset += $count
        }
    } finally {
        $source.Dispose()
    }

    $textOffset = 0
    if ($item.Length -gt $readBytes) {
        while ($textOffset -lt $offset -and ($buffer[$textOffset] -band 0xC0) -eq 0x80) {
            $textOffset++
        }
    }
    $text = $utf8.GetString($buffer, $textOffset, $offset - $textOffset)
    foreach ($value in $protected) {
        $text = $text.Replace($value, '[protected-fixture-url]', [StringComparison]::Ordinal)
    }
    $retained = $utf8.GetBytes($text)
    if ($retained.Length -gt $MaximumBytes) {
        $tailOffset = $retained.Length - $MaximumBytes
        while ($tailOffset -lt $retained.Length -and ($retained[$tailOffset] -band 0xC0) -eq 0x80) {
            $tailOffset++
        }
        $tail = [byte[]]::new($retained.Length - $tailOffset)
        [Array]::Copy($retained, $tailOffset, $tail, 0, $tail.Length)
        $retained = $tail
    }
    if ($item.Length -gt $readBytes -or $item.Length -gt $MaximumBytes) {
        $notice = $utf8.GetBytes("[earlier process output omitted; retained redacted tail follows]`n")
        $combined = [byte[]]::new($notice.Length + $retained.Length)
        [Array]::Copy($notice, 0, $combined, 0, $notice.Length)
        [Array]::Copy($retained, 0, $combined, $notice.Length, $retained.Length)
        $retained = $combined
    }

    $temporaryPath = "$Path.redacted-$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [System.IO.File]::WriteAllBytes($temporaryPath, $retained)
        [System.IO.File]::Move($temporaryPath, $Path, $true)
    } finally {
        if (Test-Path -LiteralPath $temporaryPath -PathType Leaf) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

function Publish-SafeAcceptanceEvidence {
    param(
        [Parameter(Mandatory)] [string] $StagingDirectory,
        [Parameter(Mandatory)] [string] $PublishedDirectory,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $ProtectedValues
    )

    $stagingRoot = (Resolve-Path -LiteralPath $StagingDirectory).Path
    $publishedRoot = [System.IO.Path]::GetFullPath($PublishedDirectory)
    if (Test-Path -LiteralPath $publishedRoot) {
        throw "Acceptance evidence destination already exists: $publishedRoot"
    }
    $stagingItem = Get-Item -LiteralPath $stagingRoot -Force
    if (-not $stagingItem.PSIsContainer -or
        ($stagingItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Acceptance evidence staging must be a regular non-reparse directory.'
    }

    $items = @(Get-ChildItem -LiteralPath $stagingRoot -Force)
    foreach ($item in $items) {
        if ($item.PSIsContainer -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            ($item.Name -cne 'windows-x64-acceptance.json' -and
                $item.Name -cnotmatch '^[0-9]{2}-[a-z0-9-]+\.(stdout|stderr)\.log$')) {
            throw "Acceptance evidence staging contains an unexpected item: $($item.Name)"
        }
        if ($item.Name -cmatch '\.(stdout|stderr)\.log$') {
            Protect-AcceptanceLog -Path $item.FullName -ProtectedValues $ProtectedValues
        } elseif ($item.Length -gt 1MB) {
            throw 'Acceptance evidence JSON exceeds the 1 MiB contract limit.'
        }
    }

    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $encodedProtectedValues = [System.Collections.Generic.List[byte[]]]::new()
    foreach ($value in $ProtectedValues) {
        if (-not [string]::IsNullOrEmpty($value)) {
            $encodedProtectedValues.Add($utf8.GetBytes($value))
        }
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $stagingRoot -File -Force)) {
        $content = [System.IO.File]::ReadAllBytes($item.FullName)
        foreach ($sequence in $encodedProtectedValues) {
            if (Test-ByteSequence -Content $content -Sequence $sequence) {
                throw "Protected fixture URL bytes remain in acceptance evidence: $($item.Name)"
            }
        }
    }

    New-Item -ItemType Directory -Path $publishedRoot | Out-Null
    foreach ($item in @(Get-ChildItem -LiteralPath $stagingRoot -File -Force)) {
        Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $publishedRoot $item.Name)
    }
}
