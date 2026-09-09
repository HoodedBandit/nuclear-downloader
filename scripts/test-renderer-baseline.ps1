[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$work = Join-Path ([IO.Path]::GetTempPath()) ("nuclear-renderer-contract-" + [guid]::NewGuid().ToString('N'))
$states = @('empty','populated','downloading','converting','cancelled','failed','interrupted','persistence-degraded','playlist-modal','update-modal')
$viewports = @(@(800,500), @(1000,700), @(1440,1000))
$scales = @(100,150)
$comparator = Join-Path $PSScriptRoot 'compare-renderer-baseline.ps1'
$shell = (Get-Process -Id $PID).Path

function Save-Json([string]$path, [object]$value) {
    [IO.File]::WriteAllText($path, (($value | ConvertTo-Json -Depth 20) + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
}
function Copy-Object([object]$value) { ($value | ConvertTo-Json -Depth 20 -Compress) | ConvertFrom-Json }
function Write-Png([string]$path, [int]$width, [int]$height, [bool]$changed=$false) {
    $bitmap = [Drawing.Bitmap]::new($width,$height,[Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $graphics=[Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.Clear([Drawing.Color]::FromArgb(255,24,28,36)); if($changed){$bitmap.SetPixel(0,0,[Drawing.Color]::Red)} }
        finally { $graphics.Dispose() }
        $bitmap.Save($path,[Drawing.Imaging.ImageFormat]::Png)
    } finally { $bitmap.Dispose() }
}
function Get-StringHash([string]$value) {
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($value)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}
function New-Manifest([string]$directory, [string]$kind, [string]$path, [string]$content) {
    $archivePath = "inputs/$kind/$path"
    $full = Join-Path $directory $archivePath
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($full)) | Out-Null
    [IO.File]::WriteAllText($full,$content,[Text.UTF8Encoding]::new($false))
    $files = @([ordered]@{path=$path;archivePath=$archivePath;size=(Get-Item -LiteralPath $full).Length;sha256=(Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant()})
    $manifest = [ordered]@{schemaVersion='renderer-input-manifest/v1';kind=$kind;aggregateHash=(Get-StringHash (ConvertTo-Json -InputObject $files -Depth 6 -Compress));files=$files}
    $manifestPath=Join-Path $directory "$kind-manifest.json"
    Save-Json $manifestPath $manifest
    [pscustomobject]@{Data=$manifest;Path=$manifestPath;Sha256=(Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()}
}
function New-Artifact([string]$directory, [string]$commit, [string]$description, [int]$onlyScale) {
    [IO.Directory]::CreateDirectory((Join-Path $directory 'screenshots')) | Out-Null
    $production=New-Manifest $directory 'production' 'nuclear-app/src/fixture.txt' 'production fixture'
    $harness=New-Manifest $directory 'harness' 'scripts/fixture.ps1' 'harness fixture'
    $scenarios = @()
    foreach($state in $states){ foreach($viewport in $viewports){ foreach($scale in @($onlyScale)){
        $factor=if($scale -eq 100){1.0}else{1.5}; $id="$state@$($viewport[0])x$($viewport[1])@$scale"; $repeats=@()
        foreach($sequence in 1,2){
            $relative="screenshots/$id-$sequence.png"; $full=Join-Path $directory $relative
            Write-Png $full ([int]($viewport[0]*$factor)) ([int]($viewport[1]*$factor))
            $semantic = [ordered]@{
                activeElementKey = 'download-button'
                elements = @(
                    [ordered]@{
                        key = 'title'; tag = 'h1'; text = 'Nuclear Downloader'
                        rect = [ordered]@{ x = 20; y = 16; width = 240; height = 32 }
                        visible = $true; enabled = $true; focused = $false
                        control = [ordered]@{ kind = $null; value = $null; checked = $null; expanded = $null; pressed = $null; selected = $null }
                    },
                    [ordered]@{
                        key = 'download-button'; tag = 'button'; text = 'Download'
                        rect = [ordered]@{ x = 20; y = 70; width = 120; height = 36 }
                        visible = $true; enabled = $true; focused = $true
                        control = [ordered]@{ kind = 'button'; value = $null; checked = $null; expanded = $null; pressed = $false; selected = $null }
                    },
                    [ordered]@{
                        key = 'cancel-button'; tag = 'button'; text = 'Cancel'
                        rect = [ordered]@{ x = 150; y = 70; width = 0; height = 19 }
                        visible = $false; enabled = $true; focused = $false
                        control = [ordered]@{ kind = 'button'; value = $null; checked = $null; expanded = $null; pressed = $false; selected = $null }
                    }
                )
                tabOrder = if ($state.EndsWith('-modal')) { @('download-button','cancel-button','download-button') } else { @('download-button','cancel-button') }
            }
            $repeats += [ordered]@{
                sequence = $sequence
                screenshot = [ordered]@{
                    path = $relative
                    sha256 = (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant()
                    width = [int]($viewport[0] * $factor)
                    height = [int]($viewport[1] * $factor)
                }
                semantic = $semantic
            }
        }
        $scenarios += [ordered]@{
            id = $id; state = $state
            viewport = [ordered]@{ width = $viewport[0]; height = $viewport[1] }
            scaling = [ordered]@{ targetPercent = $scale; deviceScaleFactor = $factor; actualWindowsScalePercent = 100; mode = 'emulated' }
            repeats = $repeats
        }
    } } }
    $artifact = [ordered]@{
        schemaVersion = 'renderer-visual-contract/v1'
        source = [ordered]@{ commit = $commit; productionHash = $production.Data.aggregateHash; description = $description }
        fixture = [ordered]@{ id = 'frontend-stage1'; version = '1' }
        environment = [ordered]@{
            os = [ordered]@{ name = 'Windows'; version = 'fixture'; architecture = 'x64' }
            browser = [ordered]@{ name = 'Chromium'; version = 'fixture'; engine = 'Blink' }
            renderer = [ordered]@{ name = 'WebView2-compatible-fixture'; version = '1' }
        }
        scenarios = $scenarios
    }
    $path=Join-Path $directory 'artifact.json'
    Save-Json $path $artifact
    $factor=if($onlyScale-eq100){1.0}else{1.5}
    $receipt=[ordered]@{
        schemaVersion='renderer-check-receipt/v1';runId="fixture-$onlyScale";suite='visual';selectedSpec='e2e/browser/visual-baseline.e2e.mjs'
        source=[ordered]@{commit=$commit;productionHash=$production.Data.aggregateHash;manifest=[ordered]@{path='production-manifest.json';sha256=$production.Sha256}}
        harness=[ordered]@{hash=$harness.Data.aggregateHash;manifest=[ordered]@{path='harness-manifest.json';sha256=$harness.Sha256}}
        inputsVerifiedAfter=$true;startedAtUtc='2026-01-01T00:00:00Z';finishedAtUtc='2026-01-01T00:00:01Z';exitCode=0;failure=$null;queueSize=1;scaleFactor=$factor;scalingMode='emulated'
        capture=[ordered]@{queueSize=1;scalePercent=$onlyScale;scaleFactor=$factor;scalingMode='emulated';command=@('C:\fixture\node.exe','node_modules/@wdio/cli/bin/wdio.js','run','./e2e/wdio.browser.conf.mjs','--spec','./e2e/browser/visual-baseline.e2e.mjs','--outputDir',[IO.Path]::GetFullPath($directory))}
        environment=[ordered]@{windowsBuild='fixture';actualWindowsScalePercent=100;profileRoot=(Join-Path ([IO.Path]::GetFullPath($directory)) 'profiles');profile=(Join-Path ([IO.Path]::GetFullPath($directory)) 'profiles\fixture')}
        executables=[ordered]@{
            node=[ordered]@{path='C:\fixture\node.exe';size=1;sha256=('1'*64);version='fixture'}
            browser=[ordered]@{path='C:\fixture\browser.exe';size=1;sha256=('2'*64);version='fixture'}
            driver=[ordered]@{path='C:\fixture\driver.exe';size=1;sha256=('3'*64);version='fixture'}
        }
    }
    Save-Json (Join-Path $directory 'run-receipt.json') $receipt
    $path
}
function Refresh-Hash([object]$artifact,[int]$scenarioIndex,[int]$repeatIndex,[string]$directory){
    $repeat = $artifact.scenarios[$scenarioIndex].repeats[$repeatIndex]
    $path = Join-Path $directory $repeat.screenshot.path
    $repeat.screenshot.sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Invoke-Comparison([string[]]$baseline,[string[]]$candidate,[bool]$requireNative=$false){
    $args=@('-NoProfile','-File',$comparator,'-BaselineArtifact',($baseline -join ','),'-CandidateArtifact',($candidate -join ','))
    if($requireNative){$args+='-RequireNativeScaling'}
    $output=& $shell @args 2>&1 | Out-String
    [pscustomobject]@{ExitCode=$LASTEXITCODE;Output=$output}
}
function Invoke-BaselineValidation([string[]]$baseline) {
    $args=@('-NoProfile','-File',$comparator,'-BaselineArtifact',($baseline -join ','),'-ValidateBaselineOnly')
    $output=& $shell @args 2>&1 | Out-String
    [pscustomobject]@{ExitCode=$LASTEXITCODE;Output=$output}
}
function Assert-Result([string]$name,[object]$result,[int]$exitCode,[string]$contains){
    if ($result.ExitCode -ne $exitCode -or ($contains -and -not $result.Output.Contains($contains))) {
        throw "Fixture '$name' failed. Exit=$($result.ExitCode), expected=$exitCode, output=$($result.Output)"
    }
    Write-Host "PASS $name"
}

try {
    Add-Type -AssemblyName System.Drawing
    $baseline100Dir=Join-Path $work 'baseline-100'
    $baseline150Dir=Join-Path $work 'baseline-150'
    $candidate100Dir=Join-Path $work 'candidate-100'
    $candidate150Dir=Join-Path $work 'candidate-150'
    $baseline=@(
        New-Artifact $baseline100Dir ('1'*40) 'baseline fixture' 100
        New-Artifact $baseline150Dir ('1'*40) 'baseline fixture' 150
    )
    $candidate=@(
        New-Artifact $candidate100Dir ('2'*40) 'candidate fixture' 100
        New-Artifact $candidate150Dir ('2'*40) 'candidate fixture' 150
    )
    # Change the encoded bytes without changing decoded pixels; hashes remain truthful.
    [IO.File]::AppendAllText((Join-Path $candidate100Dir 'screenshots/empty@800x500@100-1.png'),'fixture-padding',[Text.Encoding]::ASCII)
    $candidateData=Get-Content -LiteralPath $candidate[0] -Raw | ConvertFrom-Json
    Refresh-Hash $candidateData 0 0 $candidate100Dir
    Save-Json $candidate[0] $candidateData
    $match=Invoke-Comparison $baseline $candidate
    Assert-Result 'matching decoded pixels with different source and PNG bytes' $match 0 '"passed": true'
    Assert-Result 'emulated capture reports native gate incomplete' $match 0 '"nativeScalingQualified": false'
    Assert-Result 'baseline-only stability purpose' (Invoke-BaselineValidation $baseline) 0 '"purpose": "baseline-stability"'
    Assert-Result 'same artifacts cannot self-qualify' (Invoke-Comparison $baseline $baseline) 1 'same artifact path'
    Assert-Result 'native scaling acceptance switch' (Invoke-Comparison $baseline $candidate $true) 1 'native display scaling was required'

    $cases=@(
        @{Name='changed pixel';Text='decoded pixels differ';Mutate={param($a,$d) $p=Join-Path $d $a.scenarios[0].repeats[0].screenshot.path;Write-Png $p 800 500 $true;Refresh-Hash $a 0 0 $d}},
        @{Name='changed geometry';Text='semantic capture differs';Mutate={param($a,$d) $a.scenarios[0].repeats[0].semantic.elements[0].rect.x=21;$a.scenarios[0].repeats[1].semantic.elements[0].rect.x=21}},
        @{Name='changed control';Text='semantic capture differs';Mutate={param($a,$d) $a.scenarios[0].repeats[0].semantic.elements[1].enabled=$false;$a.scenarios[0].repeats[1].semantic.elements[1].enabled=$false}},
        @{Name='changed visibility';Text='semantic capture differs';Mutate={param($a,$d) $a.scenarios[0].repeats[0].semantic.elements[2].visible=$true;$a.scenarios[0].repeats[1].semantic.elements[2].visible=$true}},
        @{Name='changed copy';Text='semantic capture differs';Mutate={param($a,$d) $a.scenarios[0].repeats[0].semantic.elements[0].text='Changed';$a.scenarios[0].repeats[1].semantic.elements[0].text='Changed'}},
        @{Name='changed tab order';Text='semantic capture differs';Mutate={param($a,$d) $a.scenarios[0].repeats[0].semantic.tabOrder=@('cancel-button','download-button');$a.scenarios[0].repeats[1].semantic.tabOrder=@('cancel-button','download-button')}},
        @{Name='broken modal cycle';Text='must end at its first key';Mutate={param($a,$d) $modal=@($a.scenarios|Where-Object{$_.state-eq'playlist-modal'})[0];$modal.repeats[0].semantic.tabOrder=@('download-button','cancel-button');$modal.repeats[1].semantic.tabOrder=@('download-button','cancel-button')}},
        @{Name='missing scenario';Text='missing required scenario';Mutate={param($a,$d) $a.scenarios=@($a.scenarios | Select-Object -Skip 1)}},
        @{Name='environment mismatch';Text='environment mismatch';Mutate={param($a,$d) $a.environment.browser.version='different'}},
        @{Name='stale hash';Text='stale screenshot hash';Mutate={param($a,$d) $a.scenarios[0].repeats[0].screenshot.sha256=('0'*64)}},
        @{Name='path traversal';Text='outside the owned screenshots directory';Mutate={param($a,$d) $a.scenarios[0].repeats[0].screenshot.path='../escape.png'}},
        @{Name='dimension mismatch';Text='decoded dimensions differ from metadata';Mutate={param($a,$d) $a.scenarios[0].repeats[0].screenshot.width=799}},
        @{Name='unstable repeat baseline';Text='repeat pixels are unstable';Baseline=$true;Mutate={param($a,$d) $p=Join-Path $d $a.scenarios[0].repeats[1].screenshot.path;Write-Png $p 800 500 $true;Refresh-Hash $a 0 1 $d}}
    )
    foreach($case in $cases){
        $caseDir=Join-Path $work ($case.Name.Replace(' ', '-'));Copy-Item -LiteralPath $candidate100Dir -Destination $caseDir -Recurse
        $artifactPath = Join-Path $caseDir 'artifact.json'
        $artifact = Get-Content -LiteralPath $artifactPath -Raw | ConvertFrom-Json
        & $case.Mutate $artifact $caseDir
        Save-Json $artifactPath $artifact
        if ($case.ContainsKey('Baseline') -and $case.Baseline) {
            Assert-Result $case.Name (Invoke-Comparison @($artifactPath,$baseline[1]) $candidate) 1 $case.Text
        } else {
            Assert-Result $case.Name (Invoke-Comparison $baseline @($artifactPath,$candidate[1])) 1 $case.Text
        }
    }
    $malformedDirectory=Join-Path $work 'missing-properties'
    Copy-Item -LiteralPath $candidate100Dir -Destination $malformedDirectory -Recurse
    $malformedPath=Join-Path $malformedDirectory 'artifact.json'
    Save-Json $malformedPath ([ordered]@{schemaVersion='renderer-visual-contract/v1'})
    Assert-Result 'missing top-level properties' (Invoke-Comparison $baseline @($malformedPath,$candidate[1])) 1 'missing or unexpected properties'

    $duplicateDirectory=Join-Path $work 'duplicate-key'
    Copy-Item -LiteralPath $candidate100Dir -Destination $duplicateDirectory -Recurse
    $duplicatePath=Join-Path $duplicateDirectory 'artifact.json'
    $duplicateJson=[IO.File]::ReadAllText($duplicatePath)
    $duplicateJson=$duplicateJson.Replace('"schemaVersion": "renderer-visual-contract/v1",','"schemaVersion": "renderer-visual-contract/v1", "schemaVersion": "renderer-visual-contract/v1",')
    [IO.File]::WriteAllText($duplicatePath,$duplicateJson,[Text.UTF8Encoding]::new($false))
    Assert-Result 'duplicate JSON property' (Invoke-Comparison $baseline @($duplicatePath,$candidate[1])) 1 'duplicate JSON property'

    $oversizedDirectory=Join-Path $work 'oversized-json'
    [IO.Directory]::CreateDirectory($oversizedDirectory) | Out-Null
    $oversizedPath=Join-Path $oversizedDirectory 'artifact.json'
    $oversizedStream=[IO.File]::Create($oversizedPath)
    try { $oversizedStream.SetLength(16MB + 1) } finally { $oversizedStream.Dispose() }
    Assert-Result 'oversized JSON' (Invoke-Comparison $baseline @($oversizedPath,$candidate[1])) 1 'exceeds the 16 MiB limit'
    Write-Host 'Renderer visual contract fixtures passed (synthetic images only; no native renderer qualification).'
} finally {
    if ([IO.Directory]::Exists($work)) {
        $resolvedWork = [IO.Path]::GetFullPath($work).TrimEnd([IO.Path]::DirectorySeparatorChar)
        $expectedParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([IO.Path]::DirectorySeparatorChar)
        $actualParent = [IO.Directory]::GetParent($resolvedWork).FullName.TrimEnd([IO.Path]::DirectorySeparatorChar)
        $leaf = [IO.Path]::GetFileName($resolvedWork)
        $attributes = [IO.File]::GetAttributes($resolvedWork)
        if ($actualParent -cne $expectedParent -or -not $leaf.StartsWith('nuclear-renderer-contract-', [StringComparison]::Ordinal) -or ($attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing to remove unexpected fixture directory '$resolvedWork'"
        }
        [IO.Directory]::Delete($resolvedWork,$true)
    }
}
