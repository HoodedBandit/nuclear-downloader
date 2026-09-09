$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$runnerPath = Join-Path $PSScriptRoot 'run-renderer-check.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $runnerPath,
    [ref] $tokens,
    [ref] $parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw "Runner did not parse: $($parseErrors[0].Message)"
}
$needed = @(
    'Relative-InputPath',
    'Regular-FileIdentity',
    'Sort-OrdinalUnique',
    'Assert-RegularDirectoryTree',
    'Production-InputPaths',
    'Input-Manifest',
    'Manifests-Match'
)
$definitions = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -in $needed
}, $true))
if ($definitions.Count -ne $needed.Count) {
    throw 'Could not load the runner provenance functions.'
}
foreach ($definition in $definitions) {
    Invoke-Expression $definition.Extent.Text
}

$fixtureLeaf = "nuclear-renderer-provenance-$([Guid]::NewGuid().ToString('N'))"
$fixtureParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd(
    [IO.Path]::DirectorySeparatorChar,
    [IO.Path]::AltDirectorySeparatorChar
)
$fixtureRoot = Join-Path $fixtureParent $fixtureLeaf
$script:repositoryRoot = $fixtureRoot
$script:appRoot = Join-Path $fixtureRoot 'nuclear-app'

try {
    $sourceRoot = Join-Path $appRoot 'src'
    $staticRoot = Join-Path $appRoot 'static'
    $runRoot = Join-Path $fixtureRoot 'evidence'
    [IO.Directory]::CreateDirectory((Join-Path $sourceRoot 'lib/bindings')) | Out-Null
    [IO.Directory]::CreateDirectory($staticRoot) | Out-Null
    [IO.Directory]::CreateDirectory($runRoot) | Out-Null

    $firstPath = Join-Path $sourceRoot 'lib/first.ts'
    [IO.File]::WriteAllText($firstPath, 'export const first = 1;', [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText(
        (Join-Path $sourceRoot 'lib/first.test.ts'),
        'throw new Error("excluded test");',
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllText(
        (Join-Path $sourceRoot 'lib/AccessibleDialogHarness.svelte'),
        '<h1>excluded harness</h1>',
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllText(
        (Join-Path $sourceRoot 'lib/bindings/Generated.ts'),
        'export type Generated = string;',
        [Text.UTF8Encoding]::new($false)
    )
    [IO.File]::WriteAllBytes((Join-Path $staticRoot 'icon.bin'), [byte[]] (1, 2, 3, 4))
    foreach ($name in @('package.json', 'package-lock.json', 'jsconfig.json', 'svelte.config.js', 'vite.config.js')) {
        [IO.File]::WriteAllText((Join-Path $appRoot $name), "fixture:$name", [Text.UTF8Encoding]::new($false))
    }

    $beforePaths = @(Production-InputPaths)
    $beforeRelative = @($beforePaths | ForEach-Object { Relative-InputPath $_ })
    if ($beforeRelative -contains 'nuclear-app/src/lib/first.test.ts' -or
        $beforeRelative -contains 'nuclear-app/src/lib/AccessibleDialogHarness.svelte') {
        throw 'Production enumeration included declared test support.'
    }
    foreach ($required in @(
        'nuclear-app/jsconfig.json',
        'nuclear-app/static/icon.bin',
        'nuclear-app/src/lib/bindings/Generated.ts',
        'nuclear-app/src/lib/first.ts'
    )) {
        if ($beforeRelative -notcontains $required) {
            throw "Production enumeration missed $required."
        }
    }

    $before = Input-Manifest 'production' $beforePaths $runRoot
    $archivedFirst = Join-Path $runRoot (
        $before.files | Where-Object path -ceq 'nuclear-app/src/lib/first.ts'
    ).archivePath
    if (-not [IO.File]::Exists($archivedFirst)) {
        throw 'Preflight did not archive the first source input.'
    }

    [IO.File]::WriteAllText($firstPath, 'export const first = 2;', [Text.UTF8Encoding]::new($false))
    $afterEdit = Input-Manifest 'production' (Production-InputPaths) $null
    if (Manifests-Match $before $afterEdit) {
        throw 'A source edit was not detected by frozen replay.'
    }
    if ([IO.File]::ReadAllText($archivedFirst) -cne 'export const first = 1;') {
        throw 'The archived preflight byte stream changed with the working source.'
    }

    [IO.File]::WriteAllText($firstPath, 'export const first = 1;', [Text.UTF8Encoding]::new($false))
    $secondPath = Join-Path $sourceRoot 'second.svelte'
    [IO.File]::WriteAllText($secondPath, '<h1>added</h1>', [Text.UTF8Encoding]::new($false))
    $afterAddition = Input-Manifest 'production' (Production-InputPaths) $null
    if (Manifests-Match $before $afterAddition) {
        throw 'A source-set addition was not detected by frozen replay.'
    }

    'Renderer input provenance fixtures passed: enumeration, archived bytes, modified content, and added files.'
} finally {
    $resolvedFixture = [IO.Path]::GetFullPath($fixtureRoot)
    $resolvedParent = [IO.Path]::GetDirectoryName($resolvedFixture)
    $resolvedLeaf = [IO.Path]::GetFileName($resolvedFixture)
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals($resolvedParent, $fixtureParent) -or
        $resolvedLeaf -cne $fixtureLeaf -or
        $resolvedLeaf -notmatch '^nuclear-renderer-provenance-[a-f0-9]{32}$') {
        throw "Refusing to clean an unexpected provenance fixture path: $resolvedFixture"
    }
    if ([IO.Directory]::Exists($resolvedFixture)) {
        $fixtureItem = Get-Item -LiteralPath $resolvedFixture -Force
        if (($fixtureItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to clean a reparse provenance fixture: $resolvedFixture"
        }
        [IO.Directory]::Delete($resolvedFixture, $true)
    }
}
