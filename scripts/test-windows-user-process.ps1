$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$helper = Join-Path $PSScriptRoot 'WindowsUserProcess.cs'
Add-Type -Path $helper
$fixture = Join-Path $PSScriptRoot 'fixtures\user-process.ps1'
$pwshPath = (Get-Command pwsh.exe -ErrorAction Stop).Source
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$fixtureRoot = Join-Path $tempBase "nuclear-user-process-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
$resultPath = Join-Path $fixtureRoot 'result with spaces.json'
$previousEnvironment = $env:NUCLEAR_USER_PROCESS_TEST
try {
    $env:NUCLEAR_USER_PROCESS_TEST = 'inherited-not-profile-replaced'
    $arguments = "-NoProfile -File `"$fixture`" -Mode inspect -ResultPath `"$resultPath`" -HelperPath `"$helper`" -ExpectedText `"spaces and Unicode é漢`""
    $exitCode = [WindowsUserProcess]::Run($pwshPath, $arguments, $fixtureRoot, 30)
    if ($exitCode -ne 23) { throw "Worker exit code was not preserved: $exitCode" }
    $result = Get-Content -Raw -LiteralPath $resultPath | ConvertFrom-Json
    if ($result.integrityRid -ne 8192 -or $result.administrator -ne $false -or
        $result.environment -cne $env:NUCLEAR_USER_PROCESS_TEST -or
        $result.argument -cne 'spaces and Unicode é漢' -or $result.directory -cne $fixtureRoot) {
        throw "Normal-user token, arguments, environment, or working directory contract failed: $result"
    }
    Write-Host "Launcher integrity transition verified: $([WindowsUserProcess]::IntegrityRid()) -> $($result.integrityRid), administrator=$($result.administrator)."
    Remove-Item -LiteralPath $resultPath

    foreach ($mode in @('tree', 'timeout')) {
        $arguments = "-NoProfile -File `"$fixture`" -Mode $mode -ResultPath `"$resultPath`""
        $timedOut = $false
        $limit = if ($mode -eq 'timeout') { 5 } else { 30 }
        try {
            $exitCode = [WindowsUserProcess]::Run($pwshPath, $arguments, $fixtureRoot, $limit)
            if ($exitCode -ne 0) { throw "Tree fixture failed: $exitCode" }
        } catch {
            if ($_.Exception.InnerException -isnot [TimeoutException]) { throw }
            $timedOut = $true
        }
        if ($timedOut -ne ($mode -eq 'timeout')) { throw 'Worker timeout was not reported correctly.' }
        $childId = [int](Get-Content -Raw -LiteralPath $resultPath)
        $child = Get-Process -Id $childId -ErrorAction SilentlyContinue
        if ($child) {
            try {
                if (-not $child.WaitForExit(10000)) { throw "Owned $mode descendant survived launcher exit." }
            } finally { $child.Dispose() }
        }
        Remove-Item -LiteralPath $resultPath
    }
    $spawnRejected = $false
    try { [void][WindowsUserProcess]::Run($resultPath, '', $fixtureRoot, 5) } catch { $spawnRejected = $true }
    if (-not $spawnRejected) { throw 'Launcher accepted a missing executable.' }
    Write-Output 'Normal-user launcher passed token, quoting, environment, exit, timeout, and descendant cleanup tests.'
} finally {
    $env:NUCLEAR_USER_PROCESS_TEST = $previousEnvironment
    if (Test-Path -LiteralPath $resultPath) { Remove-Item -LiteralPath $resultPath -Force }
    [IO.Directory]::Delete($fixtureRoot, $false)
}
