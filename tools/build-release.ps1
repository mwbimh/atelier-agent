[CmdletBinding()]
param(
    [string]$TargetDir = "C:\tmp\atelier-release-target",
    [string]$OutputDir = "",
    [switch]$CleanOutput,
    [switch]$CleanTargetAfterCopy
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $repoRoot "release"
}

$targetPath = [System.IO.Path]::GetFullPath($TargetDir)
$outputPath = [System.IO.Path]::GetFullPath($OutputDir)
if ($outputPath -eq $repoRoot) {
    throw "OutputDir must not be the repository root"
}

if (Test-Path -LiteralPath $outputPath) {
    $entries = @(Get-ChildItem -LiteralPath $outputPath -Force)
    if ($entries.Count -gt 0 -and -not $CleanOutput) {
        throw "OutputDir is not empty: $outputPath. Pass -CleanOutput to replace it."
    }
    if ($CleanOutput) {
        Remove-Item -LiteralPath $outputPath -Recurse -Force
    }
}

New-Item -ItemType Directory -Path $outputPath -Force | Out-Null
$env:CARGO_TARGET_DIR = $targetPath

& cargo build --locked --profile release-dist -p atelier-pager-bin --bin ate -j 1
if ($LASTEXITCODE -ne 0) {
    throw "Release build failed with exit code $LASTEXITCODE"
}

$source = Join-Path $targetPath "release-dist\ate.exe"
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Release build completed but ate.exe is missing: $source"
}

$destination = Join-Path $outputPath "ate.exe"
Copy-Item -LiteralPath $source -Destination $destination -Force

$outputEntries = @(Get-ChildItem -LiteralPath $outputPath -Force)
if ($outputEntries.Count -ne 1 -or $outputEntries[0].Name -ne "ate.exe" -or $outputEntries[0].PSIsContainer) {
    throw "Release output contract violated: $outputPath must contain only ate.exe"
}

& $destination --version
if ($LASTEXITCODE -ne 0) {
    throw "Built ate.exe failed its version smoke test"
}

if ($CleanTargetAfterCopy) {
    & cargo clean --target-dir $targetPath
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to clean isolated Cargo target: $targetPath"
    }
}

Write-Host "Release ready: $destination"
