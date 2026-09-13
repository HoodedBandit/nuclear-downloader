Set-StrictMode -Version Latest

function Get-NuclearContainedPath {
    param([string] $Candidate, [string] $Root)
    $rootFull = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($Root))
    $candidateFull = [IO.Path]::GetFullPath($Candidate)
    $relative = [IO.Path]::GetRelativePath($rootFull, $candidateFull)
    if ([IO.Path]::IsPathRooted($relative) -or $relative -eq '..' -or
        $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)", [StringComparison]::Ordinal) -or
        $relative.StartsWith("..$([IO.Path]::AltDirectorySeparatorChar)", [StringComparison]::Ordinal)) {
        throw "Path must stay inside $rootFull"
    }
    return $candidateFull
}

function Test-NuclearPathOverlap {
    param([string] $First, [string] $Second)
    $a = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($First))
    $b = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($Second))
    $separator = [IO.Path]::DirectorySeparatorChar
    return $a.Equals($b, [StringComparison]::OrdinalIgnoreCase) -or
        $a.StartsWith("$b$separator", [StringComparison]::OrdinalIgnoreCase) -or
        $b.StartsWith("$a$separator", [StringComparison]::OrdinalIgnoreCase)
}

function Assert-NuclearExactGitRoot {
    param([Parameter(Mandatory)] [string] $RepositoryRoot)
    $expected = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($RepositoryRoot))
    $actual = [string](& git -C $expected rev-parse --show-toplevel 2>$null)
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($actual)) {
        throw 'Could not determine the exact packaged-build Git root.'
    }
    $actual = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($actual.Trim()))
    if (-not $actual.Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Packaged-build source root is not the exact Git worktree root: $expected"
    }
}

function Assert-NuclearNoReparseComponents {
    param(
        [Parameter(Mandatory)] [string] $Candidate,
        [Parameter(Mandatory)] [string] $Root
    )
    $rootFull = Get-NuclearContainedPath -Candidate $Root -Root $Root
    $candidateFull = Get-NuclearContainedPath -Candidate $Candidate -Root $rootFull
    $relative = [IO.Path]::GetRelativePath($rootFull, $candidateFull)
    $cursor = $rootFull
    $components = @('') + @($relative.Split(
        @([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar),
        [StringSplitOptions]::RemoveEmptyEntries
    ))
    foreach ($component in $components) {
        if ($component) { $cursor = Join-Path $cursor $component }
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Packaged-build paths must not traverse a reparse point: $cursor"
            }
        }
    }
    $candidateFull
}

function Assert-NuclearExactExecutableVersion {
    param(
        [Parameter(Mandatory)] [string] $ActualVersion,
        [Parameter(Mandatory)] [string] $ExpectedVersion
    )
    if ($ActualVersion -cne $ExpectedVersion) {
        throw "Packaged executable version '$ActualVersion' does not exactly match $ExpectedVersion."
    }
}

function ConvertFrom-NuclearStrictBase64 {
    param([Parameter(Mandatory)] [string] $Value, [int] $MaximumBytes = 8192)
    $encoded = if ($Value.EndsWith("`r`n", [StringComparison]::Ordinal)) {
        $Value.Substring(0, $Value.Length - 2)
    } elseif ($Value.EndsWith("`n", [StringComparison]::Ordinal)) {
        $Value.Substring(0, $Value.Length - 1)
    } else { $Value }
    if (-not $encoded -or $encoded.Length % 4 -ne 0 -or $encoded -match '\s') {
        throw 'The embedded Tauri update public key wrapper is invalid.'
    }
    try { $decoded = [Convert]::FromBase64String($encoded) }
    catch { throw 'The embedded Tauri update public key wrapper is invalid.' }
    if ($decoded.Length -gt $MaximumBytes -or [Convert]::ToBase64String($decoded) -cne $encoded) {
        throw 'The embedded Tauri update public key wrapper is invalid.'
    }
    $decoded
}

function Assert-NuclearTauriUpdatePublicKey {
    param([Parameter(Mandatory)] [string] $PublicKey, [Parameter(Mandatory)] [string] $Label)
    $decoded = ConvertFrom-NuclearStrictBase64 $PublicKey
    try { $text = [Text.UTF8Encoding]::new($false, $true).GetString($decoded) }
    catch { throw "The embedded $Label updater public key is not valid UTF-8." }
    $match = [regex]::Match($text, '\Auntrusted comment:[^\r\n]*\r?\n([A-Za-z0-9+/]+={0,2})\r?\n?\z')
    if (-not $match.Success) { throw "The embedded $Label updater public key is invalid." }
    try { $packet = [Convert]::FromBase64String($match.Groups[1].Value) }
    catch { throw "The embedded $Label updater public key is invalid." }
    if ($packet.Length -ne 42 -or $packet[0] -ne 0x45 -or $packet[1] -ne 0x64 -or
        [Convert]::ToBase64String($packet) -cne $match.Groups[1].Value) {
        throw "The embedded $Label updater public key is invalid."
    }
}

function Get-NuclearUpdateKeyConfiguration {
    param(
        [AllowEmptyString()] [string] $CurrentId,
        [AllowEmptyString()] [string] $CurrentPublicKey,
        [AllowEmptyString()] [string] $NextId,
        [AllowEmptyString()] [string] $NextPublicKey
    )
    if ([string]::IsNullOrEmpty($CurrentId) -or [string]::IsNullOrEmpty($CurrentPublicKey)) {
        throw 'Release builds require NUCLEAR_UPDATE_KEY_ID and NUCLEAR_UPDATE_PUBLIC_KEY so update manifests are authenticated.'
    }
    if ([string]::IsNullOrEmpty($NextId) -ne [string]::IsNullOrEmpty($NextPublicKey)) {
        throw 'The next updater key ID and public key must be configured together.'
    }
    if ($CurrentId -cnotmatch '^[A-Za-z0-9._-]{1,64}$') {
        throw "The current updater key ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
    }
    if ($NextId -and $NextId -cnotmatch '^[A-Za-z0-9._-]{1,64}$') {
        throw "The next updater key ID must use 1-64 ASCII letters, digits, '.', '_', or '-'."
    }
    if ($NextId -and $NextId -ceq $CurrentId) { throw 'The current and next updater key IDs must be different.' }
    Assert-NuclearTauriUpdatePublicKey $CurrentPublicKey 'current'
    if ($NextPublicKey) { Assert-NuclearTauriUpdatePublicKey $NextPublicKey 'next' }
    $utf8 = [Text.UTF8Encoding]::new($false)
    $currentHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($utf8.GetBytes($CurrentPublicKey))).ToLowerInvariant()
    $nextHash = if ($NextPublicKey) { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($utf8.GetBytes($NextPublicKey))).ToLowerInvariant() } else { $null }
    [ordered]@{
        current = [ordered]@{ id=$CurrentId; publicKeySha256=$currentHash }
        next = if ($NextId) { [ordered]@{ id=$NextId; publicKeySha256=$nextHash } } else { $null }
    }
}

function Assert-NuclearPackagedConfiguration {
    param(
        [Parameter(Mandatory)] [object] $Configuration,
        [Parameter(Mandatory)] [string] $ExpectedVersion
    )
    if ([string]$Configuration.version -cne $ExpectedVersion) {
        throw "Tauri configuration version '$($Configuration.version)' does not match $ExpectedVersion."
    }
    if ([string]$Configuration.build.frontendDist -cne '../build') {
        throw 'The packaged build must use the embedded production frontend at ../build; a URL or other path is forbidden.'
    }
    if ([string]$Configuration.build.beforeBuildCommand -cne 'npm run build') {
        throw 'The packaged build must run the reviewed production frontend command: npm run build.'
    }
    $devUri = $null
    if (-not [Uri]::TryCreate([string]$Configuration.build.devUrl, [UriKind]::Absolute, [ref]$devUri) -or
        $devUri.Scheme -cne 'http' -or $devUri.Host -cne 'localhost' -or $devUri.Port -ne 1420) {
        throw 'The development URL contract is unexpected; refusing to infer production behavior from an altered configuration.'
    }
}
