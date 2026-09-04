param(
    [ValidateSet('inspect', 'tree', 'timeout', 'child')] [string] $Mode,
    [string] $ResultPath,
    [string] $HelperPath,
    [string] $ExpectedText
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Mode -eq 'child') {
    [System.IO.File]::WriteAllText($ResultPath, "$PID")
    Start-Sleep -Seconds 120
    exit 0
}
if ($Mode -eq 'inspect') {
    Add-Type -Path $HelperPath
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    $result = @{
        integrityRid = [WindowsUserProcess]::IntegrityRid()
        administrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        environment = $env:NUCLEAR_USER_PROCESS_TEST
        argument = $ExpectedText
        directory = (Get-Location).Path
    }
    [System.IO.File]::WriteAllText($ResultPath, ($result | ConvertTo-Json))
    exit 23
}
$start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
foreach ($value in @('-NoProfile', '-File', $PSCommandPath, '-Mode', 'child', '-ResultPath', $ResultPath)) {
    [void]$start.ArgumentList.Add($value)
}
$child = [Diagnostics.Process]::Start($start)
try {
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $ResultPath)) {
        if ([DateTimeOffset]::UtcNow -gt $deadline) { throw 'Fixture child did not start.' }
        Start-Sleep -Milliseconds 50
    }
    if ($Mode -eq 'timeout') { Start-Sleep -Seconds 120 }
} finally { $child.Dispose() }
exit 0
