$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$runnerPath = Join-Path $PSScriptRoot 'run-renderer-check.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($runnerPath, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw "Runner did not parse: $($errors[0].Message)" }
$definition = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -ceq 'Get-ExactRepositoryCommit'
}, $true))
if ($definition.Count -ne 1) { throw 'Could not load the exact repository provenance guard.' }
Invoke-Expression $definition[0].Extent.Text

$commit = Get-ExactRepositoryCommit $repositoryRoot
if ($commit -cnotmatch '^[0-9a-f]{40}$') { throw 'Exact repository root did not return a commit.' }

$fixtureId = [Guid]::NewGuid().ToString('N')
$fixtureParent = Join-Path $repositoryRoot 'target\renderer-root-tests'
$nestedArchive = Join-Path $fixtureParent $fixtureId
$marker = Join-Path $nestedArchive '.nuclear-renderer-root-test'
[IO.Directory]::CreateDirectory($nestedArchive) | Out-Null
[IO.File]::WriteAllText($marker, $fixtureId, [Text.UTF8Encoding]::new($false))
try {
    $discoveredRoot = [string](& git -C $nestedArchive rev-parse --show-toplevel)
    if ($LASTEXITCODE -ne 0 -or
        -not [IO.Path]::GetFullPath($discoveredRoot.Trim()).Equals(
            $repositoryRoot.TrimEnd('\'),
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw 'The nested fixture did not demonstrate upward Git repository discovery.'
    }

    $mismatch = "Renderer source root is not the exact Git worktree root: $nestedArchive"
    $rejection = $null
    try { Get-ExactRepositoryCommit $nestedArchive *> $null } catch { $rejection = $_.Exception.Message }
    if ($rejection -cne $mismatch) {
        throw "Nested repository mismatch was not rejected with the exact reason. Actual: $rejection"
    }
} finally {
    $fixtureParentFull = [IO.Path]::GetFullPath($fixtureParent).TrimEnd('\')
    $fixtureFull = [IO.Path]::GetFullPath($nestedArchive)
    $relative = [IO.Path]::GetRelativePath($fixtureParentFull, $fixtureFull)
    $item = Get-Item -LiteralPath $fixtureFull -Force
    if ($relative -cne $fixtureId -or -not $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        -not (Test-Path -LiteralPath $marker -PathType Leaf) -or
        [IO.File]::ReadAllText($marker) -cne $fixtureId) {
        throw "Refusing to remove an ambiguous renderer-root fixture: $fixtureFull"
    }
    Remove-Item -LiteralPath $fixtureFull -Recurse -Force
}

$runnerText = Get-Content -Raw -LiteralPath $runnerPath
$guardOffset = $runnerText.IndexOf('$commit = Get-ExactRepositoryCommit $repositoryRoot', [StringComparison]::Ordinal)
$creationOffset = $runnerText.IndexOf('$runId = ', [StringComparison]::Ordinal)
if ($guardOffset -lt 0 -or $creationOffset -lt 0 -or $guardOffset -gt $creationOffset) {
    throw 'The provenance guard must run before renderer evidence directories are created.'
}

Write-Output 'Renderer exact Git-root provenance guard tests passed.'
