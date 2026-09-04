[CmdletBinding()]
param([Parameter(Mandatory)] [string] $RequestPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$logPath = Join-Path (Split-Path -Parent $RequestPath) 'worker.log'
$exitCode = 1
try {
    $request = Get-Content -Raw -LiteralPath $RequestPath | ConvertFrom-Json -AsHashtable
    & (Join-Path $PSScriptRoot 'run-windows-candidate-acceptance.ps1') @request *> $logPath
    $exitCode = 0
} catch {
    ($_ | Out-String) | Add-Content -LiteralPath $logPath
}
exit $exitCode
