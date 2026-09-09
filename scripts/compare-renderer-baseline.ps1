[CmdletBinding(DefaultParameterSetName='Compare')]
param(
    [Parameter(Mandatory,ParameterSetName='Compare')]
    [Parameter(Mandatory,ParameterSetName='BaselineOnly')]
    [string[]]$BaselineArtifact,
    [Parameter(Mandatory,ParameterSetName='Compare')]
    [string[]]$CandidateArtifact,
    [Parameter(Mandatory,ParameterSetName='BaselineOnly')]
    [switch]$ValidateBaselineOnly,
    [string]$ReportPath,
    [switch]$RequireNativeScaling
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$requiredStates = @('empty','populated','downloading','converting','cancelled','failed','interrupted','persistence-degraded','playlist-modal','update-modal')
$requiredViewports = @(@(800,500), @(1000,700), @(1440,1000))
$requiredScales = @(100,150)
$errors = [System.Collections.Generic.List[string]]::new()
$nativeScalingQualified = $true
$BaselineArtifact = @($BaselineArtifact | ForEach-Object { $_.Split(',',[StringSplitOptions]::RemoveEmptyEntries) })
if ($BaselineArtifact.Count -ne 2) { throw 'BaselineArtifact requires exactly two comma-separated or array values' }
if (-not $ValidateBaselineOnly) {
    $CandidateArtifact = @($CandidateArtifact | ForEach-Object { $_.Split(',',[StringSplitOptions]::RemoveEmptyEntries) })
    if ($CandidateArtifact.Count -ne 2) { throw 'CandidateArtifact requires exactly two comma-separated or array values' }
}
$comparatorPsHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
$comparatorCsPath = Join-Path $PSScriptRoot 'RendererBaseline.cs'
$comparatorCsHash = (Get-FileHash -LiteralPath $comparatorCsPath -Algorithm SHA256).Hash.ToLowerInvariant()

function Add-Error([string]$message) { $script:errors.Add($message) }
function Properties([object]$value) { @($value.PSObject.Properties.Name | Sort-Object) }
function Assert-Properties([object]$value, [string[]]$expected, [string]$where) {
    if ($null -eq $value) { Add-Error "$where is missing"; return $false }
    $actual = Properties $value
    $wanted = @($expected | Sort-Object)
    if (Compare-Object $actual $wanted) { Add-Error "$where has missing or unexpected properties (expected: $($wanted -join ', '))"; return $false }
    $true
}
function Canonical([object]$value) { ConvertTo-Json $value -Depth 20 -Compress }
function Find-DuplicateJsonName([Text.Json.JsonElement]$element, [string]$where) {
    if ($element.ValueKind -eq [Text.Json.JsonValueKind]::Object) {
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($property in $element.EnumerateObject()) {
            if (-not $names.Add($property.Name)) { return "$where.$($property.Name)" }
            $nested = Find-DuplicateJsonName $property.Value "$where.$($property.Name)"
            if ($nested) { return $nested }
        }
    } elseif ($element.ValueKind -eq [Text.Json.JsonValueKind]::Array) {
        $index = 0
        foreach ($item in $element.EnumerateArray()) { $nested = Find-DuplicateJsonName $item "$where[$index]"; if ($nested) { return $nested }; $index++ }
    }
    $null
}
function Read-Artifact([string]$path, [string]$label) {
    try { $resolved = (Resolve-Path -LiteralPath $path -ErrorAction Stop).Path }
    catch { Add-Error "$label artifact does not exist: $path"; return $null }
    if ([IO.Path]::GetExtension($resolved) -ne '.json') { Add-Error "$label artifact must be a .json file"; return $null }
    if ((Get-Item -LiteralPath $resolved).Length -gt 16MB) { Add-Error "$label artifact exceeds the 16 MiB limit"; return $null }
    try {
        $stream = [IO.File]::Open($resolved,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        try { $reader = [IO.StreamReader]::new($stream,[Text.UTF8Encoding]::new($false,$true),$true,4096,$true); try { $raw=$reader.ReadToEnd() } finally { $reader.Dispose() } } finally { $stream.Dispose() }
        $document = [Text.Json.JsonDocument]::Parse($raw)
        try { $duplicate = Find-DuplicateJsonName $document.RootElement '$'; if ($duplicate) { Add-Error "$label artifact has a duplicate JSON property: $duplicate"; return $null } } finally { $document.Dispose() }
        $data = $raw | ConvertFrom-Json
    }
    catch { Add-Error "$label artifact is not valid JSON: $($_.Exception.Message)"; return $null }
    [pscustomobject]@{ Path=$resolved; Directory=[IO.Path]::GetDirectoryName($resolved); Data=$data; Label=$label }
}
function Read-OwnedJson([string]$directory, [string]$name, [string]$label) {
    if ($name -notin @('run-receipt.json','production-manifest.json','harness-manifest.json')) {
        Add-Error "$label references an unsupported evidence filename '$name'"
        return $null
    }
    $path = Join-Path $directory $name
    if (-not [IO.File]::Exists($path)) { Add-Error "$label is missing '$name'"; return $null }
    $item = Get-Item -LiteralPath $path -Force
    if ($item.Length -gt 16MB -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { Add-Error "$label '$name' is oversized or a reparse point"; return $null }
    try {
        $stream = [IO.File]::Open($path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        try { $reader=[IO.StreamReader]::new($stream,[Text.UTF8Encoding]::new($false,$true),$true,4096,$true); try{$raw=$reader.ReadToEnd()}finally{$reader.Dispose()} } finally {$stream.Dispose()}
        $document=[Text.Json.JsonDocument]::Parse($raw)
        try { $duplicate=Find-DuplicateJsonName $document.RootElement '$'; if($duplicate){Add-Error "$label '$name' has duplicate JSON property $duplicate";return $null} } finally {$document.Dispose()}
        [pscustomobject]@{Path=$path;Data=($raw|ConvertFrom-Json);Sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()}
    } catch { Add-Error "$label '$name' is invalid JSON: $($_.Exception.Message)"; $null }
}
function Get-Utf8Sha256([string]$text) {
    $bytes=[Text.UTF8Encoding]::new($false).GetBytes($text)
    $hash=[Security.Cryptography.SHA256]::HashData($bytes)
    ([Convert]::ToHexString($hash)).ToLowerInvariant()
}
function Validate-Manifest([object]$evidence, [string]$kind, [string]$label) {
    if (-not $evidence) { return $null }
    $manifest=$evidence.Data
    if (-not (Assert-Properties $manifest @('schemaVersion','kind','aggregateHash','files') "$label $kind manifest")) { return $null }
    if ($manifest.schemaVersion -ne 'renderer-input-manifest/v1' -or $manifest.kind -ne $kind) { Add-Error "$label $kind manifest identity is invalid" }
    if ([string]$manifest.aggregateHash -notmatch '^[0-9a-f]{64}$') { Add-Error "$label $kind manifest aggregateHash is invalid" }
    $previous=$null; $seen=@{}
    foreach($file in @($manifest.files)) {
        if (-not (Assert-Properties $file @('path','archivePath','size','sha256') "$label $kind manifest file")) { continue }
        $path=[string]$file.path
        if ([string]::IsNullOrWhiteSpace($path) -or $path.Length -gt 512 -or $path.Contains('\') -or $path.Contains(':') -or $path.StartsWith('/') -or $path.Split('/') -contains '..') { Add-Error "$label $kind manifest has unsafe path '$path'" }
        if ($seen.ContainsKey($path)) { Add-Error "$label $kind manifest has duplicate path '$path'" } else {$seen[$path]=$true}
        if ($null -ne $previous -and [StringComparer]::Ordinal.Compare($previous,$path) -ge 0) { Add-Error "$label $kind manifest files are not strictly ordinal-sorted" }
        if ([long]$file.size -lt 0 -or [string]$file.sha256 -notmatch '^[0-9a-f]{64}$') { Add-Error "$label $kind manifest file metadata is invalid for '$path'" }
        $expectedArchivePath="inputs/$kind/$path"
        if ([string]$file.archivePath -cne $expectedArchivePath) { Add-Error "$label $kind manifest archivePath is not canonical for '$path'" }
        $archiveFull=[IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetDirectoryName($evidence.Path)) ([string]$file.archivePath)))
        $archiveRoot=[IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetDirectoryName($evidence.Path)) "inputs/$kind"))+[IO.Path]::DirectorySeparatorChar
        if (-not $archiveFull.StartsWith($archiveRoot,[StringComparison]::OrdinalIgnoreCase) -or -not [IO.File]::Exists($archiveFull)) {
            Add-Error "$label $kind archived input is missing or escapes its root for '$path'"
        }
        else {
            $archiveItem = Get-Item -LiteralPath $archiveFull -Force
            $cursor = $archiveItem
            $reparse = $false
            while ($null -ne $cursor -and $cursor.FullName.StartsWith([IO.Path]::GetDirectoryName($evidence.Path),[StringComparison]::OrdinalIgnoreCase)) {
                if (($cursor.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { $reparse=$true; break }
                $cursor = if ($cursor -is [IO.DirectoryInfo]) { $cursor.Parent } else { $cursor.Directory }
            }
            $archiveHash = (Get-FileHash -LiteralPath $archiveFull -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($reparse -or $archiveItem.Length -ne [long]$file.size -or $archiveHash -cne [string]$file.sha256) {
                Add-Error "$label $kind archived input integrity failed for '$path'"
            }
        }
        $previous=$path
    }
    $compact=ConvertTo-Json -InputObject @($manifest.files) -Depth 6 -Compress
    if ((Get-Utf8Sha256 $compact) -cne [string]$manifest.aggregateHash) { Add-Error "$label $kind manifest aggregateHash does not match its canonical files array" }
    $manifest
}
function Validate-Provenance([object]$artifact) {
    $label=$artifact.Label
    $receiptEvidence=Read-OwnedJson $artifact.Directory 'run-receipt.json' $label
    $productionEvidence=Read-OwnedJson $artifact.Directory 'production-manifest.json' $label
    $harnessEvidence=Read-OwnedJson $artifact.Directory 'harness-manifest.json' $label
    if (-not $receiptEvidence) { return $null }
    $receipt=$receiptEvidence.Data
    $receiptProperties=@('schemaVersion','runId','suite','selectedSpec','source','harness','inputsVerifiedAfter','startedAtUtc','finishedAtUtc','exitCode','failure','queueSize','scaleFactor','scalingMode','capture','environment','executables')
    if (-not (Assert-Properties $receipt $receiptProperties "$label receipt")) { return $null }
    if (-not (Assert-Properties $receipt.source @('commit','productionHash','manifest') "$label receipt.source")) { return $null }
    if (-not (Assert-Properties $receipt.source.manifest @('path','sha256') "$label receipt.source.manifest")) { return $null }
    if (-not (Assert-Properties $receipt.harness @('hash','manifest') "$label receipt.harness")) { return $null }
    if (-not (Assert-Properties $receipt.harness.manifest @('path','sha256') "$label receipt.harness.manifest")) { return $null }
    if (-not (Assert-Properties $receipt.environment @('windowsBuild','actualWindowsScalePercent','profileRoot','profile') "$label receipt.environment")) { return $null }
    if (-not (Assert-Properties $receipt.capture @('queueSize','scalePercent','scaleFactor','scalingMode','command') "$label receipt.capture")) { return $null }
    if (-not (Assert-Properties $receipt.executables @('node','browser','driver') "$label receipt.executables")) { return $null }
    foreach($name in @('node','browser','driver')) { if(-not(Assert-Properties $receipt.executables.$name @('path','size','sha256','version') "$label receipt.executables.$name")){return $null} }
    $production=Validate-Manifest $productionEvidence 'production' $label
    $harness=Validate-Manifest $harnessEvidence 'harness' $label
    if ($receipt.schemaVersion -ne 'renderer-check-receipt/v1' -or
        $receipt.suite -cne 'visual' -or
        $receipt.selectedSpec -cne 'e2e/browser/visual-baseline.e2e.mjs' -or
        -not $receipt.inputsVerifiedAfter -or
        [int]$receipt.exitCode -ne 0 -or
        $null -ne $receipt.failure) {
        Add-Error "$label receipt does not describe a successful verified visual run"
    }
    $started=[DateTimeOffset]::MinValue;$finished=[DateTimeOffset]::MinValue
    $startedValid = [DateTimeOffset]::TryParse([string]$receipt.startedAtUtc,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::AssumeUniversal,[ref]$started)
    $finishedValid = [DateTimeOffset]::TryParse([string]$receipt.finishedAtUtc,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::AssumeUniversal,[ref]$finished)
    if ([string]::IsNullOrWhiteSpace([string]$receipt.runId) -or -not $startedValid -or -not $finishedValid -or $finished -lt $started) {
        Add-Error "$label receipt run identity or timestamps are invalid"
    }
    if ($receipt.source.manifest.path -cne 'production-manifest.json' -or $receipt.harness.manifest.path -cne 'harness-manifest.json') { Add-Error "$label receipt manifest paths are not the fixed sibling filenames" }
    if ($productionEvidence -and $receipt.source.manifest.sha256 -cne $productionEvidence.Sha256) { Add-Error "$label production manifest byte hash differs from receipt" }
    if ($harnessEvidence -and $receipt.harness.manifest.sha256 -cne $harnessEvidence.Sha256) { Add-Error "$label harness manifest byte hash differs from receipt" }
    if ($production -and ($receipt.source.productionHash -cne $production.aggregateHash -or $artifact.Data.source.productionHash -cne $production.aggregateHash)) { Add-Error "$label production hash binding failed" }
    if ($harness -and $receipt.harness.hash -cne $harness.aggregateHash) { Add-Error "$label harness hash binding failed" }
    if ($artifact.Data.source.commit -cne $receipt.source.commit) { Add-Error "$label source commit differs from receipt" }
    if([string]$receipt.source.commit-notmatch'^[0-9a-fA-F]{40}$'-or[string]$receipt.source.productionHash-notmatch'^[0-9a-f]{64}$'-or[string]$receipt.harness.hash-notmatch'^[0-9a-f]{64}$'){Add-Error "$label receipt source or harness identity is invalid"}
    $artifactScale=@($artifact.Data.scenarios | ForEach-Object {[double]$_.scaling.deviceScaleFactor} | Select-Object -Unique)
    if ($artifactScale.Count -ne 1 -or [double]$receipt.scaleFactor -ne $artifactScale[0]) { Add-Error "$label receipt scaleFactor differs from scenarios" }
    $artifactMode=@($artifact.Data.scenarios | ForEach-Object {$_.scaling.mode} | Select-Object -Unique)
    if ($artifactMode.Count -ne 1 -or $receipt.scalingMode -cne $artifactMode[0]) { Add-Error "$label receipt scalingMode differs from scenarios" }
    if ([int]$receipt.environment.actualWindowsScalePercent -ne [int]$artifact.Data.scenarios[0].scaling.actualWindowsScalePercent) { Add-Error "$label receipt Windows scale differs from scenarios" }
    if(-not[IO.Path]::IsPathRooted([string]$receipt.environment.profileRoot)-or[string]::IsNullOrWhiteSpace([string]$receipt.environment.windowsBuild)-or[string]::IsNullOrWhiteSpace([string]$receipt.environment.profile)){Add-Error "$label receipt environment identity is incomplete"}
    $profileRoot=[IO.Path]::GetFullPath([string]$receipt.environment.profileRoot)
    $profile=[IO.Path]::GetFullPath([string]$receipt.environment.profile)
    $ownedProfiles=[IO.Path]::GetFullPath((Join-Path $artifact.Directory 'profiles'))
    if($profileRoot-cne$ownedProfiles-or-not$profile.StartsWith($profileRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){Add-Error "$label receipt profile paths are outside the run-owned profiles directory"}
    $targetPercent=[int]$artifact.Data.scenarios[0].scaling.targetPercent
    if ([int]$receipt.capture.queueSize -ne [int]$receipt.queueSize -or
        [int]$receipt.capture.queueSize -notin @(1,100,1000) -or
        [int]$receipt.capture.scalePercent -ne $targetPercent -or
        [double]$receipt.capture.scaleFactor -ne [double]$receipt.scaleFactor -or
        $receipt.capture.scalingMode -cne $receipt.scalingMode) {
        Add-Error "$label receipt capture does not match its top-level/scenario values"
    }
    $expectedCommand=@([string]$receipt.executables.node.path,'node_modules/@wdio/cli/bin/wdio.js','run','./e2e/wdio.browser.conf.mjs','--spec','./e2e/browser/visual-baseline.e2e.mjs','--outputDir',$artifact.Directory)
    if((Canonical @($receipt.capture.command))-cne(Canonical $expectedCommand)){Add-Error "$label receipt capture command is not canonical"}
    foreach($name in @('node','browser','driver')) { $exe=$receipt.executables.$name; if(-not[IO.Path]::IsPathRooted([string]$exe.path)-or[long]$exe.size-lt 0-or[string]$exe.sha256-notmatch'^[0-9a-f]{64}$'-or[string]::IsNullOrWhiteSpace([string]$exe.version)){Add-Error "$label receipt executable '$name' metadata is invalid"} }
    [pscustomobject]@{Receipt=$receipt;ReceiptHash=$receiptEvidence.Sha256;ProductionHash=$receipt.source.productionHash;HarnessHash=$receipt.harness.hash}
}
function Resolve-OwnedScreenshot([object]$artifact, [string]$relativePath, [string]$where) {
    if ([string]::IsNullOrWhiteSpace($relativePath) -or $relativePath.Length -gt 240 -or [IO.Path]::IsPathRooted($relativePath)) { Add-Error "$where screenshot path must be relative and at most 240 characters"; return $null }
    $normalized = $relativePath.Replace('\','/')
    if ($normalized.Contains(':') -or -not $normalized.StartsWith('screenshots/', [StringComparison]::Ordinal) -or $normalized.Contains('/../') -or $normalized.StartsWith('../') -or [IO.Path]::GetExtension($normalized) -ne '.png') { Add-Error "$where screenshot path is outside the owned screenshots directory: $relativePath"; return $null }
    $root = [IO.Path]::GetFullPath((Join-Path $artifact.Directory 'screenshots')) + [IO.Path]::DirectorySeparatorChar
    $full = [IO.Path]::GetFullPath((Join-Path $artifact.Directory $relativePath))
    if (-not $full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { Add-Error "$where screenshot path escapes its artifact directory"; return $null }
    if (-not [IO.File]::Exists($full)) { Add-Error "$where screenshot is missing: $relativePath"; return $null }
    if ((Get-Item -LiteralPath $full).Length -gt 32MB) { Add-Error "$where screenshot exceeds the 32 MiB limit"; return $null }
    $cursor = Get-Item -LiteralPath $full -Force
    while ($null -ne $cursor -and $cursor.FullName.StartsWith($artifact.Directory,[StringComparison]::OrdinalIgnoreCase)) {
        if (($cursor.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { Add-Error "$where screenshot path contains a reparse point"; return $null }
        $cursor = if ($cursor -is [IO.DirectoryInfo]) { $cursor.Parent } else { $cursor.Directory }
    }
    $full
}
function Read-PngHeader([string]$path, [string]$where) {
    try {
        $stream=[IO.File]::Open($path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        try { $header=[byte[]]::new(24); if($stream.Read($header,0,24)-ne 24){throw 'truncated header'} } finally {$stream.Dispose()}
        $signature=[byte[]](137,80,78,71,13,10,26,10); for($i=0;$i -lt 8;$i++){if($header[$i]-ne $signature[$i]){throw 'invalid PNG signature'}}
        if([Text.Encoding]::ASCII.GetString($header,12,4)-cne 'IHDR'){throw 'missing IHDR'}
        $width=([int]$header[16]-shl 24)-bor([int]$header[17]-shl 16)-bor([int]$header[18]-shl 8)-bor $header[19]
        $height=([int]$header[20]-shl 24)-bor([int]$header[21]-shl 16)-bor([int]$header[22]-shl 8)-bor $header[23]
        if($width -le 0 -or $height -le 0 -or $width -gt 4096 -or $height -gt 4096 -or ([long]$width*[long]$height)-gt 20000000){throw 'dimensions exceed safety limits'}
        @($width,$height)
    } catch { Add-Error "$where has an unsafe PNG header: $($_.Exception.Message)"; $null }
}
function Validate-Semantic([object]$semantic, [string]$where, [string]$state) {
    if (-not (Assert-Properties $semantic @('activeElementKey','elements','tabOrder') $where)) { return }
    if ($null -eq $semantic -or $null -eq $semantic.elements -or @($semantic.elements).Count -eq 0 -or @($semantic.elements).Count -gt 2000) { Add-Error "$where.elements must contain 1 through 2000 entries"; return }
    $keys = @{}
    $focused = @()
    foreach ($element in @($semantic.elements)) {
        if (-not (Assert-Properties $element @('key','tag','text','rect','visible','enabled','focused','control') "$where element")) { continue }
        if (-not (Assert-Properties $element.rect @('x','y','width','height') "$where element '$($element.key)'.rect")) { continue }
        if (-not (Assert-Properties $element.control @('kind','value','checked','expanded','pressed','selected') "$where element '$($element.key)'.control")) { continue }
        if ($element.key -isnot [string] -or $element.key.Length -gt 512 -or $element.tag -isnot [string] -or $element.tag.Length -gt 128 -or $element.text -isnot [string] -or $element.text.Length -gt 16384) { Add-Error "$where element identity or text has an invalid type or length" }
        foreach ($n in @('visible','enabled','focused')) { if ($element.$n -isnot [bool]) { Add-Error "$where element '$($element.key)'.$n must be boolean" } }
        foreach ($n in @('kind','value')) { if ($null -ne $element.control.$n -and $element.control.$n -isnot [string]) { Add-Error "$where element '$($element.key)'.control.$n must be string or null" } }
        foreach ($n in @('checked','expanded','pressed','selected')) { if ($null -ne $element.control.$n -and $element.control.$n -isnot [bool]) { Add-Error "$where element '$($element.key)'.control.$n must be boolean or null" } }
        if ([string]::IsNullOrWhiteSpace([string]$element.key) -or $keys.ContainsKey([string]$element.key)) { Add-Error "$where has an empty or duplicate element key '$($element.key)'" } else { $keys[[string]$element.key] = $true }
        if ($element.focused -eq $true) { $focused += [string]$element.key }
        foreach ($n in @('x','y','width','height')) { if ($element.rect.$n -isnot [ValueType]) { Add-Error "$where element '$($element.key)'.rect.$n must be numeric" } }
    }
    if (@($focused).Count -gt 1) { Add-Error "$where has more than one focused element" }
    $active = [string]$semantic.activeElementKey
    if ($active -and (-not $keys.ContainsKey($active) -or @($focused).Count -ne 1 -or $focused[0] -ne $active)) { Add-Error "$where activeElementKey does not identify the focused element" }
    if (-not $active -and @($focused).Count -ne 0) { Add-Error "$where has a focused element without activeElementKey" }
    $tabOrder=@($semantic.tabOrder)
    if($tabOrder.Count-eq0){Add-Error "$where.tabOrder must record at least one Tab target";return}
    for($i=0;$i-lt$tabOrder.Count;$i++){
        $tabKey=$tabOrder[$i]
        if($tabKey-isnot[string]-or-not$keys.ContainsKey([string]$tabKey)){Add-Error "$where.tabOrder contains unknown key '$tabKey'";continue}
        $tabElement=@($semantic.elements|Where-Object{$_.key-ceq$tabKey})[0]
        if(-not$tabElement.enabled){Add-Error "$where.tabOrder contains disabled key '$tabKey'"}
    }
    if($state.EndsWith('-modal',[StringComparison]::Ordinal)){
        if($tabOrder.Count-lt2-or$tabOrder[0]-cne$tabOrder[-1]){Add-Error "$where.tabOrder must end at its first key to prove modal focus wrap"}
        $cycle=@($tabOrder|Select-Object -First ($tabOrder.Count-1));if(@($cycle|Select-Object -Unique).Count-ne$cycle.Count){Add-Error "$where.tabOrder modal cycle contains a duplicate before wrap"}
    }elseif(@($tabOrder|Select-Object -Unique).Count-ne$tabOrder.Count){Add-Error "$where.tabOrder contains duplicate keys"}
}
function Validate-Artifact([object]$artifact) {
    if ($null -eq $artifact) { return @{} }
    $d = $artifact.Data; $label = $artifact.Label
    if (-not (Assert-Properties $d @('schemaVersion','source','fixture','environment','scenarios') $label)) { return @{} }
    if (-not (Assert-Properties $d.source @('commit','productionHash','description') "$label.source")) { return @{} }
    if (-not (Assert-Properties $d.fixture @('id','version') "$label.fixture")) { return @{} }
    if (-not (Assert-Properties $d.environment @('os','browser','renderer') "$label.environment")) { return @{} }
    if (-not (Assert-Properties $d.environment.os @('name','version','architecture') "$label.environment.os")) { return @{} }
    if (-not (Assert-Properties $d.environment.browser @('name','version','engine') "$label.environment.browser")) { return @{} }
    if (-not (Assert-Properties $d.environment.renderer @('name','version') "$label.environment.renderer")) { return @{} }
    if ($d.schemaVersion -ne 'renderer-visual-contract/v1') { Add-Error "$label schemaVersion is unsupported" }
    if ([string]$d.source.commit -notmatch '^[0-9a-fA-F]{40}$') { Add-Error "$label source.commit must be a full Git object id" }
    if ([string]$d.source.productionHash -notmatch '^[0-9a-f]{64}$') { Add-Error "$label source.productionHash must be lowercase SHA-256" }
    foreach ($s in @($d.fixture.id,$d.fixture.version,$d.environment.os.name,$d.environment.os.version,$d.environment.os.architecture,$d.environment.browser.name,$d.environment.browser.version,$d.environment.browser.engine,$d.environment.renderer.name,$d.environment.renderer.version)) { if ([string]::IsNullOrWhiteSpace([string]$s)) { Add-Error "$label has an empty pinned provenance/environment value" } }

    $map = @{}
    foreach ($scenario in @($d.scenarios)) {
        if (-not (Assert-Properties $scenario @('id','state','viewport','scaling','repeats') "$label scenario")) { continue }
        if (-not (Assert-Properties $scenario.viewport @('width','height') "$label scenario '$($scenario.id)'.viewport")) { continue }
        if (-not (Assert-Properties $scenario.scaling @('targetPercent','deviceScaleFactor','actualWindowsScalePercent','mode') "$label scenario '$($scenario.id)'.scaling")) { continue }
        $id = [string]$scenario.id
        $canonicalId = "$($scenario.state)@$($scenario.viewport.width)x$($scenario.viewport.height)@$($scenario.scaling.targetPercent)"
        if ($id -cne $canonicalId) { Add-Error "$label scenario id '$id' is not canonical '$canonicalId'" }
        if ($map.ContainsKey($id)) { Add-Error "$label has duplicate scenario '$id'" } else { $map[$id] = $scenario }
        $expectedFactor = if ([int]$scenario.scaling.targetPercent -eq 100) { 1.0 } else { 1.5 }
        if ([double]$scenario.scaling.deviceScaleFactor -ne $expectedFactor) { Add-Error "$label scenario '$id' has an incorrect deviceScaleFactor" }
        if ($scenario.scaling.mode -notin @('native','emulated')) { Add-Error "$label scenario '$id' has an invalid scaling mode" }
        if ($scenario.scaling.mode -eq 'native' -and [int]$scenario.scaling.actualWindowsScalePercent -ne [int]$scenario.scaling.targetPercent) { Add-Error "$label scenario '$id' claims native scaling but actualWindowsScalePercent differs" }
        if ($scenario.scaling.mode -ne 'native') { $script:nativeScalingQualified = $false }
        if (@($scenario.repeats).Count -ne 2) { Add-Error "$label scenario '$id' must contain exactly two repeat captures"; continue }
        $first = $null
        $sequences = [Collections.Generic.HashSet[int]]::new()
        foreach ($repeat in @($scenario.repeats)) {
            if (-not (Assert-Properties $repeat @('sequence','screenshot','semantic') "$label scenario '$id' repeat")) { continue }
            if (-not (Assert-Properties $repeat.screenshot @('path','sha256','width','height') "$label scenario '$id' repeat $($repeat.sequence).screenshot")) { continue }
            if ([int]$repeat.sequence -notin @(1,2)) { Add-Error "$label scenario '$id' repeat sequence must be 1 or 2" }
            if (-not $sequences.Add([int]$repeat.sequence)) { Add-Error "$label scenario '$id' has a duplicate repeat sequence" }
            Validate-Semantic $repeat.semantic "$label scenario '$id' repeat $($repeat.sequence).semantic" ([string]$scenario.state)
            $png = Resolve-OwnedScreenshot $artifact ([string]$repeat.screenshot.path) "$label scenario '$id' repeat $($repeat.sequence)"
            if ($png) {
                $headerDimensions = Read-PngHeader $png "$label scenario '$id' repeat $($repeat.sequence)"
                if (-not $headerDimensions) { continue }
                $hash = (Get-FileHash -LiteralPath $png -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($hash -cne [string]$repeat.screenshot.sha256) { Add-Error "$label scenario '$id' repeat $($repeat.sequence) has a stale screenshot hash" }
                try {
                    $dims = [NuclearDownloader.VisualContract.RendererBaseline]::GetDecodedDimensions($png)
                    if ($dims[0] -ne [int]$repeat.screenshot.width -or $dims[1] -ne [int]$repeat.screenshot.height) { Add-Error "$label scenario '$id' repeat $($repeat.sequence) decoded dimensions differ from metadata" }
                    if ($dims[0] -gt 4096 -or $dims[1] -gt 4096 -or ([long]$dims[0] * [long]$dims[1]) -gt 20000000) { Add-Error "$label scenario '$id' decoded dimensions exceed safety limits" }
                    if ($dims[0] -ne [int]([double]$scenario.viewport.width * [double]$scenario.scaling.deviceScaleFactor) -or $dims[1] -ne [int]([double]$scenario.viewport.height * [double]$scenario.scaling.deviceScaleFactor)) { Add-Error "$label scenario '$id' screenshot dimensions do not match viewport and deviceScaleFactor" }
                } catch { Add-Error "$label scenario '$id' repeat $($repeat.sequence) is not a decodable PNG: $($_.Exception.Message)" }
            }
            $current = [pscustomobject]@{ Png=$png; Semantic=(Canonical $repeat.semantic) }
            if ($null -eq $first) { $first = $current } else {
                if ($first.Semantic -cne $current.Semantic) { Add-Error "$label scenario '$id' repeat semantic captures are unstable" }
                if ($first.Png -and $current.Png) { $comparison = [NuclearDownloader.VisualContract.RendererBaseline]::CompareDecodedPixels($first.Png,$current.Png); if (-not $comparison.Equal) { Add-Error "$label scenario '$id' repeat pixels are unstable ($($comparison.DifferentPixels) differing pixels)" } }
            }
        }
    }
    $artifactScales = @($d.scenarios | ForEach-Object { [int]$_.scaling.targetPercent } | Sort-Object -Unique)
    $validScaleSet = $artifactScales.Count -eq 1 -and $artifactScales[0] -in $requiredScales
    if (-not $validScaleSet) { Add-Error "$label must contain exactly one complete scale" }
    foreach ($state in $requiredStates) { foreach ($viewport in $requiredViewports) { foreach ($scale in $artifactScales) { $id="$state@$($viewport[0])x$($viewport[1])@$scale"; if (-not $map.ContainsKey($id)) { Add-Error "$label is missing required scenario '$id'" } } } }
    $expectedCount = 30 * $artifactScales.Count
    if ($map.Count -ne $expectedCount) { Add-Error "$label scenario matrix has $($map.Count) scenarios; expected $expectedCount" }
    $map
}

try {
    Add-Type -AssemblyName System.Drawing.Common -ErrorAction Stop
    $probeBitmap = [Drawing.Bitmap]::new(1,1)
    $probeBitmap.Dispose()
    $drawingAssemblies = @([AppDomain]::CurrentDomain.GetAssemblies() | Where-Object { $_.GetName().Name -in @('System.Drawing.Common','System.Private.Windows.GdiPlus','System.Private.Windows.Core','System.Drawing.Primitives') } | ForEach-Object Location)
    Add-Type -Path (Join-Path $PSScriptRoot 'RendererBaseline.cs') -ReferencedAssemblies $drawingAssemblies -ErrorAction Stop
}
catch { Write-Error "Could not load decoded PNG comparator: $($_.Exception.Message)"; exit 2 }
function Validate-Group([string[]]$paths,[string]$label) {
    $artifacts = @()
    $union = @{}
    $provenance = [Collections.Generic.List[object]]::new()
    for ($index=0; $index -lt $paths.Count; $index++) {
        $artifact = Read-Artifact $paths[$index] "$label[$index]"
        if (-not $artifact) { continue }
        $map = Validate-Artifact $artifact
        $proof = if ($map.Count -eq 30) { Validate-Provenance $artifact } else { $null }
        $artifacts += $artifact
        $provenance.Add($proof)
        foreach ($id in $map.Keys) {
            if ($union.ContainsKey($id)) { Add-Error "$label has duplicate scenario '$id' across artifacts" }
            else { $union[$id] = $map[$id] }
        }
    }
    if ($artifacts.Count -ne $paths.Count) { return [pscustomobject]@{Artifacts=$artifacts;Map=$union;Provenance=$provenance;Valid=$false} }
    if ($artifacts.Count -eq 2 -and $provenance.Count -eq 2 -and $provenance[0] -and $provenance[1]) {
        foreach ($property in @('source','fixture','environment')) {
            if ((Canonical $artifacts[0].Data.$property) -cne (Canonical $artifacts[1].Data.$property)) { Add-Error "$label paired artifacts have mismatched $property" }
        }
        if ($provenance[0].HarnessHash -cne $provenance[1].HarnessHash) { Add-Error "$label paired artifacts have mismatched HarnessHash" }
        $firstEnvironment = $provenance[0].Receipt.environment
        $secondEnvironment = $provenance[1].Receipt.environment
        if ([string]$firstEnvironment.windowsBuild -cne [string]$secondEnvironment.windowsBuild -or [int]$firstEnvironment.actualWindowsScalePercent -ne [int]$secondEnvironment.actualWindowsScalePercent) { Add-Error "$label paired receipts have mismatched Windows environment" }
        foreach ($name in @('node','browser','driver')) { foreach ($property in @('size','sha256','version')) {
            if ($provenance[0].Receipt.executables.$name.$property -cne $provenance[1].Receipt.executables.$name.$property) { Add-Error "$label paired receipts have mismatched $name $property" }
        } }
    }
    foreach ($state in $requiredStates) { foreach ($viewport in $requiredViewports) { foreach ($scale in $requiredScales) {
        $id="$state@$($viewport[0])x$($viewport[1])@$scale"
        if (-not $union.ContainsKey($id)) { Add-Error "$label union is missing '$id'" }
    } } }
    if ($union.Count -ne 60) { Add-Error "$label union must contain exactly 60 scenarios; found $($union.Count)" }
    $proofCount = @($provenance | Where-Object { $null -ne $_ }).Count
    [pscustomobject]@{Artifacts=$artifacts;Map=$union;Provenance=$provenance;Valid=($artifacts.Count -eq $paths.Count -and $union.Count -eq 60 -and $proofCount -eq $paths.Count)}
}
function Report-Inputs([object]$group){
    $result=@()
    for($i=0;$i-lt$group.Artifacts.Count;$i++){
        $artifact=$group.Artifacts[$i];$proof=if($i-lt$group.Provenance.Count){$group.Provenance[$i]}else{$null}
        $sourceProperty=$artifact.Data.PSObject.Properties['source']
        $source=if($sourceProperty){$sourceProperty.Value}else{$null}
        $result+=[ordered]@{path=$artifact.Path;artifactSha256=(Get-FileHash -LiteralPath $artifact.Path -Algorithm SHA256).Hash.ToLowerInvariant();sourceCommit=if($source){$source.commit}else{$null};productionHash=if($source){$source.productionHash}else{$null};harnessHash=if($proof){$proof.HarnessHash}else{$null};receiptSha256=if($proof){$proof.ReceiptHash}else{$null}}
    }
    @($result)
}
$baselineGroup=Validate-Group $BaselineArtifact 'baseline'
$candidateGroup=$null
if(-not$ValidateBaselineOnly){$candidateGroup=Validate-Group $CandidateArtifact 'candidate'}
if($candidateGroup){
    $baselinePaths=@($baselineGroup.Artifacts|ForEach-Object{$_.Path})
    foreach($artifact in $candidateGroup.Artifacts){if($artifact.Path-in$baselinePaths){Add-Error 'baseline and candidate groups resolve to the same artifact path'}}
    if($baselineGroup.Valid-and$candidateGroup.Valid){
        foreach($property in @('fixture','environment')){if((Canonical $baselineGroup.Artifacts[0].Data.$property)-cne(Canonical $candidateGroup.Artifacts[0].Data.$property)){Add-Error "baseline/candidate $property mismatch"}}
        if($baselineGroup.Valid-and$candidateGroup.Valid){
            if($baselineGroup.Provenance[0].HarnessHash-cne$candidateGroup.Provenance[0].HarnessHash){Add-Error 'baseline/candidate harness manifest mismatch'}
            if([string]$baselineGroup.Provenance[0].Receipt.environment.windowsBuild-cne[string]$candidateGroup.Provenance[0].Receipt.environment.windowsBuild-or[int]$baselineGroup.Provenance[0].Receipt.environment.actualWindowsScalePercent-ne[int]$candidateGroup.Provenance[0].Receipt.environment.actualWindowsScalePercent){Add-Error 'baseline/candidate Windows environment mismatch'}
            foreach($name in @('node','browser','driver')){foreach($property in @('size','sha256','version')){if($baselineGroup.Provenance[0].Receipt.executables.$name.$property-cne$candidateGroup.Provenance[0].Receipt.executables.$name.$property){Add-Error "baseline/candidate $name $property mismatch"}}}
        }
    }
    if($baselineGroup.Valid-and$candidateGroup.Valid){foreach ($id in @($baselineGroup.Map.Keys)) {
        if (-not $candidateGroup.Map.ContainsKey($id)) { continue }
        $b=$baselineGroup.Map[$id]; $c=$candidateGroup.Map[$id]
        foreach ($property in @('state','viewport','scaling')) { if ((Canonical $b.$property) -cne (Canonical $c.$property)) { Add-Error "scenario '$id' baseline/candidate $property mismatch" } }
        if (@($b.repeats).Count -eq 2 -and @($c.repeats).Count -eq 2) {
            if ((Canonical $b.repeats[0].semantic) -cne (Canonical $c.repeats[0].semantic)) { Add-Error "scenario '$id' semantic capture differs from baseline" }
            $baselineOwner=@($baselineGroup.Artifacts|Where-Object{$_.Data.scenarios.id-contains$id})[0]
            $candidateOwner=@($candidateGroup.Artifacts|Where-Object{$_.Data.scenarios.id-contains$id})[0]
            $bp=Resolve-OwnedScreenshot $baselineOwner ([string]$b.repeats[0].screenshot.path) "baseline scenario '$id'"
            $cp=Resolve-OwnedScreenshot $candidateOwner ([string]$c.repeats[0].screenshot.path) "candidate scenario '$id'"
            if ($bp -and $cp -and (Read-PngHeader $bp "baseline scenario '$id'") -and (Read-PngHeader $cp "candidate scenario '$id'")) { $comparison=[NuclearDownloader.VisualContract.RendererBaseline]::CompareDecodedPixels($bp,$cp); if (-not $comparison.Equal) { Add-Error "scenario '$id' decoded pixels differ from baseline ($($comparison.DifferentPixels) pixels; first at $($comparison.FirstDifferenceX),$($comparison.FirstDifferenceY))" } }
        }
    }}
}
if ($RequireNativeScaling -and -not $nativeScalingQualified) { Add-Error 'native display scaling was required, but one or more captures used emulated scaling' }
$candidateInputReport = [object[]]::new(0)
if ($candidateGroup) { $candidateInputReport = [object[]]@(Report-Inputs $candidateGroup) }
$report = [ordered]@{
    schemaVersion='renderer-visual-comparison/v1'
    purpose=if($ValidateBaselineOnly){'baseline-stability'}else{'baseline-candidate-parity'}
    comparedCandidate=(-not[bool]$ValidateBaselineOnly)
    passed=($errors.Count -eq 0)
    nativeScalingQualified=$nativeScalingQualified
    baselineInputs=@(Report-Inputs $baselineGroup)
    candidateInputs=$candidateInputReport
    comparator=[ordered]@{powershellSha256=$comparatorPsHash;csharpSha256=$comparatorCsHash}
    errors=@($errors)
}
$json = $report | ConvertTo-Json -Depth 5
if ($ReportPath) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($ReportPath), $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false)) }
$json
if ($errors.Count -ne 0) { exit 1 }
