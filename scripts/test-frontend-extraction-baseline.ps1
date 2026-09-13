$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$runner = Join-Path $PSScriptRoot 'check-frontend-extraction.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($runner, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw "Frontend extraction runner did not parse: $($errors[0].Message)" }
$definition = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -ceq 'Resolve-BaselineArtifacts'
}, $true))
if ($definition.Count -ne 1) { throw 'Could not load the frontend extraction baseline resolver.' }
Invoke-Expression $definition[0].Extent.Text

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureParent = [IO.Path]::GetFullPath((Join-Path $repositoryRoot 'target'))
$fixtureId = 'frontend-baseline-contract-' + [Guid]::NewGuid().ToString('N')
$fixture = Join-Path $fixtureParent $fixtureId
$marker = Join-Path $fixture '.nuclear-frontend-baseline-contract'
[IO.Directory]::CreateDirectory($fixture) | Out-Null
[IO.File]::WriteAllText($marker, $fixtureId, [Text.UTF8Encoding]::new($false))
try {
    $missingDefaultsRejected = $false
    try { Resolve-BaselineArtifacts $fixture $null | Out-Null } catch { $missingDefaultsRejected = $true }
    if (-not $missingDefaultsRejected) {
        throw 'Missing historical defaults were accepted.'
    }

    $firstRelative = 'target/renderer-checks/visual-20260909T060041Z-885155a599d74d619e7da52667d0c328/visual-100.json'
    $secondRelative = 'target/renderer-checks/visual-20260909T055802Z-eacf8baed77645b5b8e066b3d261ab8d/visual-150.json'
    $first = [IO.Path]::GetFullPath((Join-Path $fixture $firstRelative))
    $second = [IO.Path]::GetFullPath((Join-Path $fixture $secondRelative))
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($first)) | Out-Null
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($second)) | Out-Null
    [IO.File]::WriteAllText($first, '{}', [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($second, '{}', [Text.UTF8Encoding]::new($false))

    $defaults = @(Resolve-BaselineArtifacts $fixture $null)
    if ($defaults.Count -ne 2 -or $defaults[0] -cne $first -or $defaults[1] -cne $second) {
        throw 'Omitted BaselineArtifact did not resolve to the isolated historical artifacts in order.'
    }
    $override = @(Resolve-BaselineArtifacts $fixture @($firstRelative, $secondRelative))
    if ($override.Count -ne 2 -or $override[0] -cne $first -or $override[1] -cne $second) {
        throw 'Relative baseline artifacts were not resolved from the supplied repository root.'
    }
    foreach ($invalid in @(
        (, @($first)),
        (, @($first, $first)),
        (, @($first, (Join-Path $fixture 'missing.json'))),
        (, @($first, (Join-Path $fixture 'wrong.txt')))
    )) {
        $rejected = $false
        try { Resolve-BaselineArtifacts $fixture $invalid | Out-Null } catch { $rejected = $true }
        if (-not $rejected) { throw 'Invalid baseline input was accepted.' }
    }
} finally {
    $fixtureParentFull = $fixtureParent.TrimEnd([IO.Path]::DirectorySeparatorChar)
    $fixtureFull = [IO.Path]::GetFullPath($fixture)
    $relative = [IO.Path]::GetRelativePath($fixtureParentFull, $fixtureFull)
    $item = Get-Item -LiteralPath $fixtureFull -Force
    if ($relative -cne $fixtureId -or -not $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        -not (Test-Path -LiteralPath $marker -PathType Leaf) -or
        [IO.File]::ReadAllText($marker) -cne $fixtureId) {
        throw "Refusing to remove an ambiguous frontend baseline fixture: $fixtureFull"
    }
    Remove-Item -LiteralPath $fixtureFull -Recurse -Force
}

Write-Output 'Frontend extraction baseline input contract tests passed.'
