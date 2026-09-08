[CmdletBinding()]
param(
    [string]$Filter = '',
    [ValidateRange(1, 32)][int]$TestThreads = 1,
    [switch]$IncludeIgnored
)

$ErrorActionPreference = 'Stop'
$taskRepositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskTemporaryParent = Join-Path $taskRepositoryRoot 'nuclear-app\src-tauri\target\regression-temp'
$taskTemporaryRoot = Join-Path $taskTemporaryParent ([Guid]::NewGuid().ToString())
$taskMarker = Join-Path $taskTemporaryRoot '.nuclear-regression-owned'
$taskOriginalTemp = $env:TEMP
$taskOriginalTmp = $env:TMP
$taskExitCode = 1

New-Item -ItemType Directory -Path $taskTemporaryRoot -Force | Out-Null
Set-Content -LiteralPath $taskMarker -Value 'schemaVersion=1' -NoNewline
try {
    $env:TEMP = $taskTemporaryRoot
    $env:TMP = $taskTemporaryRoot
    $taskArguments = @(
        'test', '--manifest-path', (Join-Path $taskRepositoryRoot 'nuclear-app\src-tauri\Cargo.toml'),
        '--locked', '--offline', '--all-features'
    )
    if ($Filter) { $taskArguments += $Filter }
    $taskArguments += @('--', "--test-threads=$TestThreads")
    if ($IncludeIgnored) { $taskArguments += '--ignored' }
    & cargo @taskArguments
    $taskExitCode = $LASTEXITCODE
}
finally {
    $env:TEMP = $taskOriginalTemp
    $env:TMP = $taskOriginalTmp
    if ($taskExitCode -eq 0) {
        $taskResolvedParent = [IO.Path]::GetFullPath($taskTemporaryParent).TrimEnd('\') + '\'
        $taskResolvedRoot = [IO.Path]::GetFullPath($taskTemporaryRoot)
        if (-not $taskResolvedRoot.StartsWith($taskResolvedParent, [StringComparison]::OrdinalIgnoreCase) -or
            -not (Test-Path -LiteralPath $taskMarker -PathType Leaf) -or
            (Get-Content -LiteralPath $taskMarker -Raw) -cne 'schemaVersion=1') {
            throw 'Refusing to remove an unverified regression directory.'
        }
        Remove-Item -LiteralPath $taskResolvedRoot -Recurse -Force
    }
    else {
        Write-Warning "Regression evidence preserved at $taskTemporaryRoot"
    }
}
exit $taskExitCode
