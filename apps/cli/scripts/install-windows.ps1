# Installer for the Atelier Windows release package and managed PowerShell runtime.
#
# Keep this script next to ate.exe, then run:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install-windows.ps1
#
# Optional:
#   -InstallDir C:\Tools\Atelier  Install to a custom directory.
#   -NoPathUpdate                 Do not add the install directory to User PATH.
#   -SetupSandbox                 Run `ate.exe sandbox setup` after installation.
#   -PowerShellArchive <zip>      Install an offline portable PowerShell archive.
#   -PowerShellArchiveSha256 <h>  Required SHA-256 for the offline archive.

[CmdletBinding()]
param(
    [string]$InstallDir = "",
    [switch]$NoPathUpdate,
    [switch]$SetupSandbox,
    [string]$PowerShellVersion = "7.6.4",
    [string]$PowerShellArchive = "",
    [string]$PowerShellArchiveSha256 = "",
    [switch]$SkipPowerShellRuntime
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

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Value
    )
    [IO.File]::WriteAllText($Path, $Value, [Text.UTF8Encoding]::new($false))
}

function Set-AtelierReadExecuteAcl {
    param([Parameter(Mandatory = $true)][string]$Path)
    & icacls.exe $Path /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to apply protected read/execute ACLs to $Path"
    }
}

if (-not $SkipPowerShellRuntime -and -not (Test-IsAdministrator)) {
    $elevatedArgs = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $PSCommandPath,
        "-InstallDir", $InstallDir,
        "-PowerShellVersion", $PowerShellVersion
    )
    if ($NoPathUpdate) { $elevatedArgs += "-NoPathUpdate" }
    if ($SetupSandbox) { $elevatedArgs += "-SetupSandbox" }
    if (-not [string]::IsNullOrWhiteSpace($PowerShellArchive)) {
        $elevatedArgs += @("-PowerShellArchive", $PowerShellArchive)
        $elevatedArgs += @("-PowerShellArchiveSha256", $PowerShellArchiveSha256)
    }
    Write-Host "Administrator approval is required to install the managed PowerShell runtime."
    $process = Start-Process -FilePath "powershell.exe" -Verb RunAs -Wait -PassThru -ArgumentList $elevatedArgs
    if ($process.ExitCode -ne 0) {
        throw "Elevated Atelier installation failed with exit code $($process.ExitCode)"
    }
    exit 0
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

function Install-ManagedPowerShell {
    param(
        [Parameter(Mandatory = $true)][string]$Version,
        [string]$Archive,
        [string]$ArchiveSha256
    )

    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) {
        throw "ProgramData is not set; cannot install the managed PowerShell runtime"
    }
    if ($Version -notmatch '^\d+\.\d+\.\d+$') {
        throw "PowerShell version must use numeric major.minor.patch format"
    }
    $architecture = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
    $archiveName = "PowerShell-$PowerShellVersion-win-$architecture.zip"
    $runtimeRoot = Join-Path $env:ProgramData "Atelier\runtimes\powershell"
    $versionRoot = Join-Path $runtimeRoot $Version
    $activeManifest = Join-Path $runtimeRoot "active.json"
    New-Item -ItemType Directory -Path $runtimeRoot -Force | Out-Null
    Set-AtelierReadExecuteAcl -Path $runtimeRoot

    $temporaryDownload = $null
    $checksumsDownload = $null
    try {
        if ([string]::IsNullOrWhiteSpace($Archive)) {
            $temporaryDownload = Join-Path ([IO.Path]::GetTempPath()) ("atelier-{0}" -f $archiveName)
            $checksumsDownload = Join-Path ([IO.Path]::GetTempPath()) ("atelier-powershell-{0}-hashes.sha256" -f $Version)
            $releaseBase = "https://github.com/PowerShell/PowerShell/releases/download/v$Version"
            Invoke-WebRequest -UseBasicParsing -Uri "$releaseBase/$archiveName" -OutFile $temporaryDownload
            Invoke-WebRequest -UseBasicParsing -Uri "$releaseBase/hashes.sha256" -OutFile $checksumsDownload
            $expectedLine = Get-Content -LiteralPath $checksumsDownload | Where-Object {
                $_ -match ("(?i)^[0-9a-f]{64}\s+\*?" + [regex]::Escape($archiveName) + "$")
            } | Select-Object -First 1
            if (-not $expectedLine) {
                throw "The PowerShell release checksum manifest does not contain $archiveName"
            }
            $expectedHash = ($expectedLine -split "\s+")[0].ToUpperInvariant()
            $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $temporaryDownload).Hash.ToUpperInvariant()
            if ($actualHash -ne $expectedHash) {
                throw "PowerShell archive SHA-256 mismatch"
            }
            $Archive = $temporaryDownload
        } else {
            $Archive = [IO.Path]::GetFullPath($Archive)
            if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
                throw "PowerShell archive does not exist: $Archive"
            }
            if ($ArchiveSha256 -notmatch "(?i)^[0-9a-f]{64}$") {
                throw "-PowerShellArchiveSha256 must be the archive's 64-character SHA-256"
            }
            $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Archive).Hash.ToUpperInvariant()
            if ($actualHash -ne $ArchiveSha256.ToUpperInvariant()) {
                throw "Offline PowerShell archive SHA-256 mismatch"
            }
        }

        $staging = Join-Path $runtimeRoot (".staging-{0}-{1}" -f $Version, $PID)
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Path $staging -Force | Out-Null
        try {
            Expand-Archive -LiteralPath $Archive -DestinationPath $staging -Force
            $pwsh = Join-Path $staging "pwsh.exe"
            if (-not (Test-Path -LiteralPath $pwsh -PathType Leaf)) {
                throw "The PowerShell archive is not self-contained: pwsh.exe is missing"
            }
            if ((Get-Item -LiteralPath $pwsh -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "The PowerShell runtime contains a reparse-point pwsh.exe"
            }
            $signature = Get-AuthenticodeSignature -LiteralPath $pwsh
            if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
                $signature.SignerCertificate.Subject -notmatch "Microsoft") {
                throw "The managed PowerShell executable does not have a valid Microsoft signature"
            }
            & $pwsh -NoProfile -NonInteractive -Command "if (`$PSVersionTable.PSVersion.Major -lt 7) { exit 1 }"
            if ($LASTEXITCODE -ne 0) {
                throw "The managed PowerShell runtime failed its startup probe"
            }

            $backupRoot = "$versionRoot.backup-$PID"
            Remove-Item -LiteralPath $backupRoot -Recurse -Force -ErrorAction SilentlyContinue
            if (Test-Path -LiteralPath $versionRoot) {
                Move-Item -LiteralPath $versionRoot -Destination $backupRoot
            }
            try {
                Move-Item -LiteralPath $staging -Destination $versionRoot
                Remove-Item -LiteralPath $backupRoot -Recurse -Force -ErrorAction SilentlyContinue
            } catch {
                Remove-Item -LiteralPath $versionRoot -Recurse -Force -ErrorAction SilentlyContinue
                if (Test-Path -LiteralPath $backupRoot) {
                    Move-Item -LiteralPath $backupRoot -Destination $versionRoot
                }
                throw
            }
        } catch {
            Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
            throw
        }

        Set-AtelierReadExecuteAcl -Path $versionRoot
        foreach ($sandboxAccount in @("AtelierSandbox", "AtelierSandboxNoNet")) {
            if (Get-LocalUser -Name $sandboxAccount -ErrorAction SilentlyContinue) {
                & icacls.exe $versionRoot /grant:r ("{0}:(OI)(CI)RX" -f $sandboxAccount) | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "Failed to grant $sandboxAccount access to the managed PowerShell runtime"
                }
            }
        }

        $installedPwsh = Join-Path $versionRoot "pwsh.exe"
        $manifestTemporary = "$activeManifest.new.$PID"
        $manifest = @{
            schema_version = 1
            version = $Version
            architecture = $architecture
            path = $installedPwsh
        } | ConvertTo-Json
        Write-Utf8NoBom -Path $manifestTemporary -Value $manifest
        Move-Item -LiteralPath $manifestTemporary -Destination $activeManifest -Force
        Write-Host "Managed PowerShell installed: $installedPwsh"
    } finally {
        if ($temporaryDownload) { Remove-Item -LiteralPath $temporaryDownload -Force -ErrorAction SilentlyContinue }
        if ($checksumsDownload) { Remove-Item -LiteralPath $checksumsDownload -Force -ErrorAction SilentlyContinue }
    }
}

function Write-ToolchainRegistry {
    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) { return }
    $toolRoot = Join-Path $env:ProgramData "Atelier\tools"
    New-Item -ItemType Directory -Path $toolRoot -Force | Out-Null
    Set-AtelierReadExecuteAcl -Path $toolRoot
    $roots = @()

    $activeManifest = Join-Path $env:ProgramData "Atelier\runtimes\powershell\active.json"
    if (Test-Path -LiteralPath $activeManifest -PathType Leaf) {
        $managed = Get-Content -LiteralPath $activeManifest -Raw | ConvertFrom-Json
        $managedRoot = Split-Path -Parent ([IO.Path]::GetFullPath([string]$managed.path))
        $roots += @{ path = $managedRoot; enabled = $true }
    }

    $approvedPrefixes = @(
        $env:ProgramW6432,
        $env:ProgramFiles,
        ${env:ProgramFiles(x86)},
        $env:SystemRoot,
        (Join-Path $env:ProgramData "Atelier\tools")
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object {
        [IO.Path]::GetFullPath($_).TrimEnd('\') + '\'
    }
    foreach ($name in @("git.exe", "rg.exe", "fd.exe", "jq.exe", "curl.exe", "tar.exe", "7z.exe", "dotnet.exe")) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $command) { continue }
        $path = [IO.Path]::GetFullPath($command.Source)
        if ($path -match "(?i)\\WindowsApps\\") { continue }
        $approved = $approvedPrefixes | Where-Object {
            $path.StartsWith($_, [StringComparison]::OrdinalIgnoreCase)
        } | Select-Object -First 1
        if (-not $approved) { continue }
        $root = Split-Path -Parent $path
        if ((Get-Item -LiteralPath $root -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        if (-not ($roots | Where-Object { [string]::Equals($_.path, $root, [StringComparison]::OrdinalIgnoreCase) })) {
            $roots += @{ path = $root; enabled = $true }
        }
    }
    $registry = @{ schema_version = 1; roots = $roots } | ConvertTo-Json -Depth 4
    $registryPath = Join-Path $toolRoot "registry.json"
    $temporary = "$registryPath.new.$PID"
    Write-Utf8NoBom -Path $temporary -Value $registry
    Move-Item -LiteralPath $temporary -Destination $registryPath -Force
}

if (-not $SkipPowerShellRuntime) {
    Install-ManagedPowerShell -Version $PowerShellVersion -Archive $PowerShellArchive -ArchiveSha256 $PowerShellArchiveSha256
    Write-ToolchainRegistry
}

if ($SetupSandbox) {
    & $destination sandbox setup
    if ($LASTEXITCODE -ne 0) {
        throw "Atelier was installed, but sandbox setup failed with exit code $LASTEXITCODE"
    }
}

Write-Host "Atelier installed: $destination"
Write-Host "Run 'ate' to configure a Provider and select a model."
