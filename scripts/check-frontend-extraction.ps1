[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidatePattern('^[a-z][a-z0-9-]{0,60}$')] [string] $Stage,
    [Parameter(Mandatory)] [string] $ChromeBinary,
    [Parameter(Mandatory)] [string] $ChromeDriverBinary,
    [Parameter(Mandatory)] [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')] [string] $ExpectedBrowserVersion,
    [string[]] $BaselineArtifact
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

function Resolve-BaselineArtifacts(
    [string] $RepositoryRoot,
    [string[]] $RequestedArtifacts
) {
    $defaults = @(
        'target/renderer-checks/visual-20260909T060041Z-885155a599d74d619e7da52667d0c328/visual-100.json',
        'target/renderer-checks/visual-20260909T055802Z-eacf8baed77645b5b8e066b3d261ab8d/visual-150.json'
    )
    $requested = if ($null -eq $RequestedArtifacts -or $RequestedArtifacts.Count -eq 0) {
        $defaults
    } else {
        @($RequestedArtifacts)
    }
    if ($requested.Count -ne 2) {
        throw 'BaselineArtifact requires exactly two JSON artifact paths.'
    }
    $resolved = @($requested | ForEach-Object {
        if ([string]::IsNullOrWhiteSpace($_)) {
            throw 'BaselineArtifact paths must be non-empty.'
        }
        $path = if ([IO.Path]::IsPathRooted($_)) { $_ } else { Join-Path $RepositoryRoot $_ }
        $path = [IO.Path]::GetFullPath($path)
        if ([IO.Path]::GetExtension($path) -cne '.json' -or
            -not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "BaselineArtifact must name an existing JSON file: $path"
        }
        $path
    })
    if ($resolved[0].Equals($resolved[1], [StringComparison]::OrdinalIgnoreCase)) {
        throw 'BaselineArtifact requires two distinct JSON artifact paths.'
    }
    return $resolved
}

$baseline = @(Resolve-BaselineArtifacts $repositoryRoot $BaselineArtifact)
$appRoot = Join-Path $repositoryRoot 'nuclear-app'
$node = Join-Path $appRoot 'src-tauri/target/toolchains/node-v22.23.1-win-x64/node.exe'
$runId = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N')
$logRoot = Join-Path $repositoryRoot "target/frontend-extractions/$Stage-$runId"
[IO.Directory]::CreateDirectory($logRoot) | Out-Null
function Check-Node([string] $Name, [string[]] $Arguments) {
    $log = Join-Path $logRoot "$Name.log"
    & $node @Arguments *> $log
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath $log -Tail 40
        throw "$Name failed; evidence: $log"
    }
    Write-Host "Passed $Name"
}

function Check-PowerShell([string] $Name, [string] $Script) {
    $log = Join-Path $logRoot "$Name.log"
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot $Script) *> $log
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath $log -Tail 40
        throw "$Name failed; evidence: $log"
    }
    Write-Host "Passed $Name"
}

Push-Location $appRoot
try {
    Check-Node 'source-health-fixtures' @('--test', (Join-Path $repositoryRoot 'scripts/source-health-frontend.test.mjs'))
    Check-Node 'source-health' @((Join-Path $repositoryRoot 'scripts/source-health-frontend.mjs'))
    Check-Node 'frontend-inventory-fixtures' @('--test', (Join-Path $repositoryRoot 'scripts/frontend-source-inventory.test.mjs'))
    Check-Node 'frontend-inventory-check' @((Join-Path $repositoryRoot 'scripts/frontend-source-inventory.mjs'), '--check')
    Check-PowerShell 'renderer-source-root' 'test-renderer-source-root.ps1'
    Check-PowerShell 'renderer-soak-metrics-contract' 'test-renderer-soak-metrics-contract.ps1'
    Check-Node 'svelte-sync' @('node_modules/@sveltejs/kit/svelte-kit.js', 'sync')
    Check-Node 'typecheck' @('node_modules/svelte-check/bin/svelte-check', '--tsconfig', './jsconfig.json')
    Check-Node 'lint' @('node_modules/eslint/bin/eslint.js', '.', '--max-warnings', '0')
    Check-Node 'format' @('node_modules/prettier/bin/prettier.cjs', '--check', '.')
    Check-Node 'unit' @('node_modules/vitest/vitest.mjs', 'run')
    Check-Node 'build' @('node_modules/vite/bin/vite.js', 'build')
    Check-Node 'production-exclusion' @('e2e/verify-production-bundle.mjs')
} finally {
    Pop-Location
}

$browserArguments = @{
    ChromeBinary = $ChromeBinary
    ChromeDriverBinary = $ChromeDriverBinary
    ExpectedBrowserVersion = $ExpectedBrowserVersion
}
$candidate = @()
foreach ($capture in @(
    @{ Suite = 'workflows'; Scale = 100 },
    @{ Suite = 'visual'; Scale = 100 },
    @{ Suite = 'visual'; Scale = 150 }
)) {
    $label = "$($capture.Suite)-$($capture.Scale)"
    $log = Join-Path $logRoot "$label.log"
    & (Join-Path $PSScriptRoot 'run-renderer-check.ps1') -Suite $capture.Suite -ScalePercent $capture.Scale @browserArguments *> $log
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath $log -Tail 50
        throw "$label failed; evidence: $log"
    }
    $markers = @(Select-String -LiteralPath $log -Pattern '^RENDERER_EVIDENCE=(.+)$')
    if ($markers.Count -ne 1) { throw "$label did not report one evidence directory." }
    $evidence = $markers[0].Matches[0].Groups[1].Value
    if ($capture.Suite -eq 'visual') {
        $candidate += Join-Path $evidence "visual-$($capture.Scale).json"
    }
    Write-Host "Passed $label`: $evidence"
}

$comparisonPath = Join-Path $repositoryRoot "docs/internal-cleanup-$Stage-visual.json"
& (Join-Path $PSScriptRoot 'compare-renderer-baseline.ps1') -BaselineArtifact $baseline -CandidateArtifact $candidate -ReportPath $comparisonPath *> (Join-Path $logRoot 'comparison.log')
if ($LASTEXITCODE -ne 0) {
    Get-Content -LiteralPath (Join-Path $logRoot 'comparison.log') -Tail 50
    throw "Visual comparison failed; evidence: $logRoot"
}
Write-Host "Passed all extraction gates. Evidence: $logRoot"
Write-Host "Visual comparison: $comparisonPath"
