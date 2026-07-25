# Offline installer for the Atelier Windows release package.
#
# Keep this script next to ate.exe, then run:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install-windows.ps1
#
# Optional:
#   -InstallDir C:\Tools\Atelier  Install to a custom directory.
#   -NoPathUpdate                 Do not add the install directory to User PATH.
#   -SetupSandbox                 Run `ate.exe sandbox setup` after installation.

[CmdletBinding()]
param(
    [string]$InstallDir = "",
    [switch]$NoPathUpdate,
    [switch]$SetupSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($PSVersionTable.ContainsKey("Platform") -and
    $PSVersionTable["Platform"] -ne "Win32NT") {
    throw "install-windows.ps1 can only install the Windows release"
}
if ([string]::IsNullOrWhiteSpace($PSScriptRoot)) {
    throw "Run install-windows.ps1 from the extracted release directory"
}

$source = Join-Path $PSScriptRoot 'ate.exe'
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "The release package is incomplete: ate.exe is missing next to install-windows.ps1"
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        throw "USERPROFILE is not set; pass -InstallDir explicitly"
    }
    $InstallDir = Join-Path $env:USERPROFILE ".atelier\bin"
}

$source = [System.IO.Path]::GetFullPath($source)
$installPath = [System.IO.Path]::GetFullPath($InstallDir)
$destination = Join-Path $installPath "ate.exe"
New-Item -ItemType Directory -Path $installPath -Force | Out-Null

$sameFile = [string]::Equals(
    $source,
    [System.IO.Path]::GetFullPath($destination),
    [System.StringComparison]::OrdinalIgnoreCase
)

if (-not $sameFile) {
    $temporary = Join-Path $installPath ("ate.new.{0}.exe" -f $PID)
    $backup = Join-Path $installPath "ate.exe.old"
    Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue

    try {
        Copy-Item -LiteralPath $source -Destination $temporary -Force
        & $temporary --version
        if ($LASTEXITCODE -ne 0) {
            throw "The packaged ate.exe failed its version smoke test"
        }

        if (Test-Path -LiteralPath $destination -PathType Leaf) {
            Move-Item -LiteralPath $destination -Destination $backup -Force
        }
        Move-Item -LiteralPath $temporary -Destination $destination -Force
        Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    } catch {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path -LiteralPath $destination -PathType Leaf) -and
            (Test-Path -LiteralPath $backup -PathType Leaf)) {
            Move-Item -LiteralPath $backup -Destination $destination -Force
        }
        throw
    }
} else {
    & $destination --version
    if ($LASTEXITCODE -ne 0) {
        throw "The packaged ate.exe failed its version smoke test"
    }
}

if (-not $NoPathUpdate) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = if ([string]::IsNullOrWhiteSpace($userPath)) {
        @()
    } else {
        @($userPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    }
    $alreadyPresent = $entries | Where-Object {
        [string]::Equals(
            $_.TrimEnd("\"),
            $installPath.TrimEnd("\"),
            [System.StringComparison]::OrdinalIgnoreCase
        )
    }
    if (-not $alreadyPresent) {
        $newPath = (@($installPath) + $entries) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "Added $installPath to User PATH."
    }
    if (($env:Path -split ";") -notcontains $installPath) {
        $env:Path = "$installPath;$env:Path"
    }
}

if ($SetupSandbox) {
    & $destination sandbox setup
    if ($LASTEXITCODE -ne 0) {
        throw "Atelier was installed, but sandbox setup failed with exit code $LASTEXITCODE"
    }
}

Write-Host "Atelier installed: $destination"
Write-Host "Run 'ate' to configure a Provider and select a model."
