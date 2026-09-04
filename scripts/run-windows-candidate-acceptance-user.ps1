[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $CandidateDirectory,
    [Parameter(Mandatory)] [string] $ResultsDirectory,
    [ValidatePattern('^0\.6\.0$')] [string] $ExpectedVersion = '0.6.0',
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-f]{40}$')] [string] $ExpectedCommitSha,
    [Parameter(Mandatory)] [ValidatePattern('^[1-9][0-9]*$')] [string] $ExpectedCandidateRunId
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -cne 'true' -or [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw 'Candidate acceptance requires the disposable GitHub Actions Windows account.'
}
Add-Type -Path (Join-Path $PSScriptRoot 'WindowsUserProcess.cs')
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$candidateRoot = (Resolve-Path -LiteralPath $CandidateDirectory).Path
$resultsRoot = [System.IO.Path]::GetFullPath($ResultsDirectory)
if (Test-Path -LiteralPath $resultsRoot) { throw 'Acceptance results directory already exists.' }
$launchRoot = Join-Path $env:RUNNER_TEMP "nuclear-acceptance-launch-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $launchRoot | Out-Null
$requestPath = Join-Path $launchRoot 'request.json'
$logPath = Join-Path $launchRoot 'worker.log'
try {
    $request = @{
        CandidateDirectory = $candidateRoot
        ResultsDirectory = $resultsRoot
        ExpectedVersion = $ExpectedVersion
        ExpectedCommitSha = $ExpectedCommitSha
        ExpectedCandidateRunId = $ExpectedCandidateRunId
    }
    [System.IO.File]::WriteAllText($requestPath, ($request | ConvertTo-Json), [System.Text.UTF8Encoding]::new($false))
    $pwshPath = (Get-Command pwsh.exe -ErrorAction Stop).Source
    $workerPath = Join-Path $PSScriptRoot 'run-windows-candidate-acceptance-worker.ps1'
    $arguments = "-NoLogo -NoProfile -NonInteractive -File `"$workerPath`" -RequestPath `"$requestPath`""
    $exitCode = [WindowsUserProcess]::Run($pwshPath, $arguments, $repositoryRoot, 1800)
    if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath -Tail 120 | Write-Host }
    if ($exitCode -ne 0) { throw "Normal-user candidate acceptance failed (exit $exitCode)." }
    & (Join-Path $PSScriptRoot 'verify-acceptance-evidence.ps1') `
        -EvidenceDirectory $resultsRoot -CandidateDirectory $candidateRoot `
        -ExpectedVersion $ExpectedVersion -ExpectedCommitSha $ExpectedCommitSha `
        -ExpectedCandidateRunId $ExpectedCandidateRunId
} finally {
    if (-not (Test-Path -LiteralPath $resultsRoot)) { New-Item -ItemType Directory -Path $resultsRoot | Out-Null }
    if (Test-Path -LiteralPath $logPath) {
        $source = [System.IO.File]::OpenRead($logPath)
        try {
            $length = [int][Math]::Min($source.Length, 4MB)
            [void]$source.Seek(-$length, [System.IO.SeekOrigin]::End)
            $tail = [byte[]]::new($length)
            $source.ReadExactly($tail, 0, $length)
            [System.IO.File]::WriteAllBytes((Join-Path $resultsRoot '00-user-integrity.stdout.log'), $tail)
        } finally { $source.Dispose() }
    }
    # Delete only our two exact files; unexpected contents make nonrecursive cleanup fail.
    foreach ($path in @($requestPath, $logPath)) {
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    }
    [System.IO.Directory]::Delete($launchRoot, $false)
}
