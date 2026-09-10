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
    'Harness-InputPaths',
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
    $functionText = $definition.Extent.Text
    if ($definition.Name -ceq 'Harness-InputPaths') {
        $quotedRunnerPath = "'$($runnerPath.Replace("'", "''"))'"
        $functionText = $functionText.Replace('$PSCommandPath', $quotedRunnerPath)
    }
    Invoke-Expression $functionText
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
    $supportRoot = Join-Path $appRoot 'e2e/browser/support'
    $nativeRoot = Join-Path $appRoot 'e2e/native'
    $runRoot = Join-Path $fixtureRoot 'evidence'
    [IO.Directory]::CreateDirectory((Join-Path $sourceRoot 'lib/bindings')) | Out-Null
    [IO.Directory]::CreateDirectory($staticRoot) | Out-Null
    [IO.Directory]::CreateDirectory($supportRoot) | Out-Null
    [IO.Directory]::CreateDirectory($nativeRoot) | Out-Null
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
    $sharedHelperPath = Join-Path $nativeRoot 'helpers.mjs'
    [IO.File]::WriteAllText(
        $sharedHelperPath,
        'export const editQueuedFilename = 1;',
        [Text.UTF8Encoding]::new($false)
    )

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

    $workflowHarnessPaths = @(Harness-InputPaths 'e2e/browser/renderer-workflows.e2e.mjs')
    $workflowHarnessRelative = @($workflowHarnessPaths | ForEach-Object { Relative-InputPath $_ })
    if ($workflowHarnessRelative -notcontains 'nuclear-app/e2e/native/helpers.mjs') {
        throw 'Workflow harness enumeration missed the shared native helper.'
    }
    $visualHarnessRelative = @(
        Harness-InputPaths 'e2e/browser/visual-baseline.e2e.mjs' |
            ForEach-Object { Relative-InputPath $_ }
    )
    if ($visualHarnessRelative -contains 'nuclear-app/e2e/native/helpers.mjs') {
        throw 'An unrelated renderer harness included the workflow-only shared native helper.'
    }

    $helperBefore = Input-Manifest 'harness' @($sharedHelperPath) $runRoot
    $helperEntry = @($helperBefore.files)[0]
    $expectedHelperHash = (Get-FileHash -LiteralPath $sharedHelperPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($helperEntry.path -cne 'nuclear-app/e2e/native/helpers.mjs' -or
        $helperEntry.sha256 -cne $expectedHelperHash) {
        throw 'The shared native helper was not recorded with its exact content hash.'
    }
    $archivedHelper = Join-Path $runRoot $helperEntry.archivePath
    if (-not [IO.File]::Exists($archivedHelper)) {
        throw 'The shared native helper was not archived with the workflow harness.'
    }

    [IO.File]::WriteAllText(
        $sharedHelperPath,
        'export const editQueuedFilename = 2;',
        [Text.UTF8Encoding]::new($false)
    )
    $helperAfterEdit = Input-Manifest 'harness' @($sharedHelperPath) $null
    if (Manifests-Match $helperBefore $helperAfterEdit) {
        throw 'A shared native helper mutation was not detected by frozen harness replay.'
    }

    'Renderer input provenance fixtures passed: production enumeration, archived bytes, modified content, added files, and shared workflow helper coverage.'
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
