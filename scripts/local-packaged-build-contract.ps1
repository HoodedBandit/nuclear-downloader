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
