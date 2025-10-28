param(
    [switch]$SkipBuild,
    [ValidateSet("release", "debug")]
    [string]$Profile = "release",
    [string]$SignThumbprint = "",
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [switch]$UseLocalMachineStore,
    [string]$SignToolPath = "signtool.exe"
)

$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$targetExe = Join-Path $root "target/$Profile/dashboard.exe"

if (-not $SkipBuild) {
    Write-Host "Building dashboard binary (profile: $Profile)…" -ForegroundColor Cyan
    cargo build --bin dashboard $(if ($Profile -eq "release") { "--release" })
}

if (-not (Test-Path $targetExe)) {
    throw "Expected dashboard executable at $targetExe. Build step may have failed."
}

$configPath = Join-Path $root "packaging/Velopack.toml"
if (-not (Test-Path $configPath)) {
    throw "Missing Velopack configuration at $configPath."
}

function Get-TomlString {
    param(
        [string[]]$Lines,
        [string]$Key
    )

    $pattern = "^\s*{0}\s*=\s*""(?<value>.+?)""" -f [regex]::Escape($Key)
    foreach ($line in $Lines) {
        $match = [regex]::Match($line, $pattern)
        if ($match.Success) { return $match.Groups['value'].Value }
    }
    throw "Failed to read '$Key' from Velopack.toml"
}

function Get-TomlArray {
    param(
        [string[]]$Lines,
        [string]$Key
    )

    $pattern = "^\s*{0}\s*=\s*\[(?<value>.+)\]" -f [regex]::Escape($Key)
    foreach ($line in $Lines) {
        $match = [regex]::Match($line, $pattern)
        if ($match.Success) {
            $rawValues = $match.Groups['value'].Value -split ','
            return @(
                $rawValues | ForEach-Object {
                    $_.Trim() -replace '^\"', '' -replace '\"$', ''
                } | Where-Object { $_ -ne '' }
            )
        }
    }
    throw "Failed to read '$Key' from Velopack.toml"
}

$configLines = Get-Content $configPath
$packId = Get-TomlString -Lines $configLines -Key 'id'
$packTitle = Get-TomlString -Lines $configLines -Key 'title'
$packVersion = Get-TomlString -Lines $configLines -Key 'version'
$workingDirName = Get-TomlString -Lines $configLines -Key 'workingDir'
$outputDirName = Get-TomlString -Lines $configLines -Key 'output'
$mainExe = Get-TomlString -Lines $configLines -Key 'mainExe'
$authorsList = Get-TomlArray -Lines $configLines -Key 'authors'

$bundleDir = Join-Path $root (Join-Path 'packaging' $workingDirName)
$distDir = Join-Path $root (Join-Path 'packaging' $outputDirName)

if (Test-Path $bundleDir) {
    Remove-Item $bundleDir -Recurse -Force
}
New-Item -ItemType Directory -Path $bundleDir | Out-Null

if (-not (Test-Path $distDir)) {
    New-Item -ItemType Directory -Path $distDir | Out-Null
}

$configDir = Split-Path $configPath -Parent
$copyEntries = @()
$i = 0
while ($i -lt $configLines.Length) {
    $line = $configLines[$i].Trim()
    if ($line -eq '[[build.copy]]') {
        $entry = @{ source = $null; destination = $null }
        $i++
        while ($i -lt $configLines.Length) {
            $inner = $configLines[$i].Trim()
            if ($inner -match '^source\s*=\s*"(?<src>.+?)"') {
                $entry.source = $Matches['src']
            } elseif ($inner -match '^destination\s*=\s*"(?<dest>.+?)"') {
                $entry.destination = $Matches['dest']
            } elseif ($inner -like '[[]*') {
                $i--
                break
            } elseif ([string]::IsNullOrWhiteSpace($inner)) {
                # continue reading until next section
            }
            $i++
        }
        if (-not $entry.source -or -not $entry.destination) {
            throw "Invalid [[build.copy]] entry in Velopack.toml"
        }
        $copyEntries += [PSCustomObject]$entry
    }
    $i++
}

if ($copyEntries.Count -eq 0) {
    Write-Warning "No [[build.copy]] entries found; defaulting to dashboard executable." 
    $copyEntries = @([PSCustomObject]@{
        source = "../target/$Profile/dashboard.exe"
        destination = $mainExe
    })
}

foreach ($entry in $copyEntries) {
    $sourcePath = Join-Path $configDir $entry.source
    $resolvedSource = Resolve-Path $sourcePath -ErrorAction Stop
    $destPath = Join-Path $bundleDir $entry.destination
    $destParent = Split-Path $destPath -Parent
    if (-not (Test-Path $destParent)) {
        New-Item -ItemType Directory -Path $destParent -Force | Out-Null
    }
    Copy-Item -Path $resolvedSource -Destination $destPath -Force
}

Write-Host "Packing with Velopack…" -ForegroundColor Cyan
$arguments = @(
    'pack',
    '--packId', $packId,
    '--packVersion', $packVersion,
    '--packDir', $bundleDir,
    '--outputDir', $distDir,
    '--packTitle', $packTitle,
    '--packAuthors', ($authorsList -join ', '),
    '--mainExe', $mainExe,
    '--runtime', 'win-x64'
)

$shouldSign = -not [string]::IsNullOrWhiteSpace($SignThumbprint)
if ($shouldSign) {
    if (-not (Get-Command $SignToolPath -ErrorAction SilentlyContinue)) {
        throw "Unable to locate signtool executable '$SignToolPath'."
    }

    $signtoolArgsBase = @('sign', '/fd', 'SHA256')
    if ($UseLocalMachineStore) {
        $signtoolArgsBase += @('/s', 'My', '/sm')
    }
    $signtoolArgsBase += @('/sha1', $SignThumbprint)
    if (-not [string]::IsNullOrWhiteSpace($TimestampUrl)) {
        $signtoolArgsBase += @('/tr', $TimestampUrl, '/td', 'SHA256')
    }
    $signtoolArgsBase += '/v'

    $exeFiles = Get-ChildItem -Path $bundleDir -Filter *.exe -Recurse
    foreach ($exe in $exeFiles) {
        Write-Host "Signing $($exe.FullName)…" -ForegroundColor Cyan
        $signArgs = $signtoolArgsBase + @($exe.FullName)
        & $SignToolPath @signArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Signtool failed for $($exe.FullName) with exit code $LASTEXITCODE."
        }
    }

    $signParams = ($signtoolArgsBase -join ' ')
    $arguments += @('--signParams', $signParams)
}

& vpk @arguments
if ($LASTEXITCODE -ne 0) {
    throw "Velopack CLI exited with code $LASTEXITCODE."
}

Write-Host "Velopack artifacts ready in $distDir" -ForegroundColor Green
