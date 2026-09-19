[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Rejected {
    param([scriptblock] $Action, [string] $Message)
    $rejected = $false
    try { & $Action *> $null } catch { $rejected = $true }
    Assert-True $rejected $Message
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildScript = Join-Path $PSScriptRoot 'build-local-app.ps1'
$contractScript = Join-Path $PSScriptRoot 'local-packaged-build-contract.ps1'
$targetRoot = Join-Path $repositoryRoot 'nuclear-app\src-tauri\target'
$probeRoot = Join-Path $targetRoot "local-packaged-preflight-$([Guid]::NewGuid().ToString('N'))"
$registeredDirectory = Join-Path $targetRoot 'install-audit-v0.5.4'

. $contractScript

$originalUpdateEnvironment = [ordered]@{
    NUCLEAR_UPDATE_KEY_ID = $env:NUCLEAR_UPDATE_KEY_ID
    NUCLEAR_UPDATE_PUBLIC_KEY = $env:NUCLEAR_UPDATE_PUBLIC_KEY
    NUCLEAR_UPDATE_NEXT_KEY_ID = $env:NUCLEAR_UPDATE_NEXT_KEY_ID
    NUCLEAR_UPDATE_NEXT_PUBLIC_KEY = $env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY
}
try {
    $validPublicKey = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXkgRTc2MjBGMTg0MkI0RTgxRgpSV1FmNkxSQ0dBOWk1M21sWWVjTzRJelQ1MVRHUHB2V3VjTlNDaDFDQk0wUVRhTG43M1k3R0ZPMw=='
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration '' '' '' '' } 'Missing release updater keys were accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'bad id' $validPublicKey '' '' } 'Malformed current key ID was accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'current' 'not-base64' '' '' } 'Malformed current public key was accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'current' $validPublicKey 'next' '' } 'A partial next-key pair was accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'current' $validPublicKey 'bad next' $validPublicKey } 'Malformed next key ID was accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'current' $validPublicKey 'next' 'not-base64' } 'Malformed next public key was accepted.'
    Assert-Rejected { Get-NuclearUpdateKeyConfiguration 'current' $validPublicKey 'current' $validPublicKey } 'Duplicate current/next key IDs were accepted.'
    $validKeys = Get-NuclearUpdateKeyConfiguration 'current' $validPublicKey '' ''
    Assert-True ($validKeys.current.id -ceq 'current' -and $validKeys.next -eq $null) 'Valid current key with empty optional rotation slots was rejected.'
    Assert-True ([string]$validKeys.current.publicKeySha256 -cmatch '^[0-9a-f]{64}$') 'Public key identity hash was not recorded.'
    $env:NUCLEAR_UPDATE_KEY_ID = 'current'
    $env:NUCLEAR_UPDATE_PUBLIC_KEY = $validPublicKey
    $env:NUCLEAR_UPDATE_NEXT_KEY_ID = ''
    $env:NUCLEAR_UPDATE_NEXT_PUBLIC_KEY = ''
    $missingKeyProbe = Join-Path $targetRoot "local-packaged-missing-key-$([Guid]::NewGuid().ToString('N'))"
    $malformedKeyProbe = Join-Path $targetRoot "local-packaged-malformed-key-$([Guid]::NewGuid().ToString('N'))"
    $env:NUCLEAR_UPDATE_KEY_ID = ''
    $env:NUCLEAR_UPDATE_PUBLIC_KEY = ''
    Assert-Rejected { & $buildScript -PreflightOnly -BuildRoot $missingKeyProbe } 'Build preflight accepted missing release updater keys.'
    Assert-True (-not (Test-Path -LiteralPath $missingKeyProbe)) 'Missing-key preflight created its output directory.'
    $env:NUCLEAR_UPDATE_KEY_ID = 'current'
    $env:NUCLEAR_UPDATE_PUBLIC_KEY = 'not-base64'
    Assert-Rejected { & $buildScript -PreflightOnly -BuildRoot $malformedKeyProbe } 'Build preflight accepted a malformed updater public key.'
    Assert-True (-not (Test-Path -LiteralPath $malformedKeyProbe)) 'Malformed-key preflight created its output directory.'
    $env:NUCLEAR_UPDATE_PUBLIC_KEY = $validPublicKey

    $validConfig = [pscustomobject]@{
        version = '0.7.9'
        build = [pscustomobject]@{
            beforeBuildCommand = 'npm run build'
            devUrl = 'http://localhost:1420'
            frontendDist = '../build'
        }
    }
    Assert-NuclearPackagedConfiguration $validConfig '0.7.9'
    $devPayloadConfig = $validConfig | ConvertTo-Json -Depth 4 | ConvertFrom-Json
    $devPayloadConfig.build.frontendDist = $devPayloadConfig.build.devUrl
    Assert-Rejected { Assert-NuclearPackagedConfiguration $devPayloadConfig '0.7.9' } `
        'A development URL was accepted as the packaged frontend payload.'
    Assert-NuclearExactExecutableVersion '0.7.9' '0.7.9'
    Assert-Rejected { Assert-NuclearExactExecutableVersion '0.7.90' '0.7.9' } `
        'Executable version validation accepted a longer version prefix.'

    $fixtureId = [Guid]::NewGuid().ToString('N')
    $fixtureRoot = Join-Path $targetRoot "local-packaged-contract-$fixtureId"
    $nestedGitProbe = Join-Path $fixtureRoot 'nested-source'
    $reparseTarget = Join-Path $fixtureRoot 'junction-target'
    $reparsePath = Join-Path $fixtureRoot 'junction-build-root'
    $fixtureMarker = Join-Path $fixtureRoot '.nuclear-local-packaged-contract-test'
    [IO.Directory]::CreateDirectory($nestedGitProbe) | Out-Null
    [IO.Directory]::CreateDirectory($reparseTarget) | Out-Null
    [IO.File]::WriteAllText($fixtureMarker, $fixtureId, [Text.UTF8Encoding]::new($false))
    try {
        $discovered = ([string](& git -C $nestedGitProbe rev-parse --show-toplevel)).Trim()
        Assert-True ($LASTEXITCODE -eq 0) 'Nested Git fixture did not discover the parent repository.'
        Assert-True ([IO.Path]::GetFullPath($discovered).Equals($repositoryRoot, [StringComparison]::OrdinalIgnoreCase)) `
            'Nested Git fixture did not resolve to the expected parent repository.'
        Assert-Rejected { Assert-NuclearExactGitRoot $nestedGitProbe } `
            'An archive-like nested directory inherited the parent repository commit.'

        New-Item -ItemType Junction -Path $reparsePath -Target $reparseTarget | Out-Null
        Assert-Rejected { Assert-NuclearNoReparseComponents $reparsePath $targetRoot } `
            'Build path validation accepted a junction component.'
        Assert-Rejected { & $buildScript -PreflightOnly -BuildRoot $reparsePath } `
            'Packaged-build preflight accepted a junction build root.'
    } finally {
        $fixtureFull = [IO.Path]::GetFullPath($fixtureRoot)
        $expectedParent = [IO.Path]::GetFullPath($targetRoot).TrimEnd('\')
        $relative = [IO.Path]::GetRelativePath($expectedParent, $fixtureFull)
        $item = Get-Item -LiteralPath $fixtureFull -Force
        if ($relative -cne "local-packaged-contract-$fixtureId" -or
            -not $item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not (Test-Path -LiteralPath $fixtureMarker -PathType Leaf) -or
            [IO.File]::ReadAllText($fixtureMarker) -cne $fixtureId) {
            throw "Refusing to remove an ambiguous packaged-build test fixture: $fixtureFull"
        }
        if (Test-Path -LiteralPath $reparsePath) { Remove-Item -LiteralPath $reparsePath -Force }
        Remove-Item -LiteralPath $fixtureFull -Recurse -Force
    }

    Assert-True (-not (Test-Path -LiteralPath $probeRoot)) 'The fresh preflight probe path unexpectedly exists.'
    $preflight = & $buildScript -PreflightOnly -BuildRoot $probeRoot
    Assert-True (-not (Test-Path -LiteralPath $probeRoot)) 'Preflight created its output directory.'
    Assert-True ([IO.Path]::GetFullPath([string]$preflight.buildRoot) -ceq [IO.Path]::GetFullPath($probeRoot)) `
        'Preflight did not return the exact canonical owned output root.'
    Assert-True ([string]$preflight.source.commit -cmatch '^[0-9a-f]{40}$') 'Preflight source commit is invalid.'
    Assert-True ([string]$preflight.source.digest -cmatch '^[0-9a-f]{64}$') 'Preflight source digest is invalid.'
    Assert-True ([string]$preflight.updateKeys.current.id -ceq 'current') 'Preflight did not report the configured current key ID.'
    Assert-True ([string]$preflight.updateKeys.current.publicKeySha256 -ceq [string]$validKeys.current.publicKeySha256) 'Preflight public-key hash differs from the validated fixture.'
    Assert-True ($null -eq $preflight.updateKeys.next) 'Preflight invented an optional next key.'
    foreach ($name in @('node','npm','tauri','rustc','cargo')) {
        $tool = $preflight.toolchains.$name
        Assert-True ([IO.Path]::IsPathFullyQualified([string]$tool.path)) "Tool path is not absolute: $name"
        Assert-True ([long]$tool.size -gt 0) "Tool size is invalid: $name"
        Assert-True ([string]$tool.sha256 -cmatch '^[0-9a-f]{64}$') "Tool hash is invalid: $name"
        Assert-True (-not [string]::IsNullOrWhiteSpace([string]$tool.version)) "Tool version is missing: $name"
    }
    $status = @(& git -C $repositoryRoot status --porcelain=v1 --untracked-files=all)
    Assert-True ([bool]$preflight.source.dirty -eq ($status.Count -ne 0)) 'Preflight dirty state differs from Git.'

    $sidecarRoot = Join-Path $repositoryRoot 'nuclear-app\src-tauri\binaries'
    $sidecarLock = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'nuclear-app\src-tauri\sidecars.lock.json') | ConvertFrom-Json
    foreach ($sidecar in $sidecarLock.sidecars) {
        $path = Join-Path $sidecarRoot ([string]$sidecar.filename)
        Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "Missing pinned sidecar: $($sidecar.filename)"
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-True ($actual -ceq [string]$sidecar.sha256) "Pinned sidecar hash mismatch: $($sidecar.filename)"
    }

    Assert-Rejected { & $buildScript -PreflightOnly -BuildRoot $registeredDirectory } `
        'The registered active installation directory was accepted as build output.'
    Assert-Rejected { & $buildScript -PreflightOnly -BuildRoot (Join-Path $repositoryRoot 'target\outside-tauri') } `
        'A build root outside the Tauri target directory was accepted.'
}
finally {
    foreach ($name in $originalUpdateEnvironment.Keys) {
        if ($null -eq $originalUpdateEnvironment[$name]) {
            Remove-Item -LiteralPath "env:$name" -ErrorAction SilentlyContinue
        }
        else {
            Set-Item -LiteralPath "env:$name" -Value $originalUpdateEnvironment[$name]
        }
    }
}

foreach ($name in $originalUpdateEnvironment.Keys) {
    $restoredItem = Get-Item -LiteralPath "env:$name" -ErrorAction SilentlyContinue
    $restored = if ($null -eq $restoredItem) { $null } else { $restoredItem.Value }
    Assert-True ($restored -ceq $originalUpdateEnvironment[$name]) "Test did not restore process environment variable $name."
}

Write-Output 'Local packaged-build preflight and fail-closed contract tests passed.'
