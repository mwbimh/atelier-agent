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
#   -SkipDefaultTools              Do not install missing Git, ripgrep, or uv.

[CmdletBinding()]
param(
    [string]$InstallDir = "",
    [switch]$NoPathUpdate,
    [switch]$SetupSandbox,
    [string]$PowerShellVersion = "7.6.4",
    [string]$PowerShellArchive = "",
    [string]$PowerShellArchiveSha256 = "",
    [switch]$SkipPowerShellRuntime,
    [switch]$SkipDefaultTools
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

function Invoke-DownloadWithRetry {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )
    $lastError = $null
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Remove-Item -LiteralPath $OutFile -Force -ErrorAction SilentlyContinue
            Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $OutFile
            return
        } catch {
            $lastError = $_
            if ($attempt -lt 3) {
                Start-Sleep -Seconds ([Math]::Pow(2, $attempt))
            }
        }
    }
    throw "Download failed after 3 attempts: $Uri`n$($lastError | Out-String)"
}

function Assert-SafeZipArchive {
    param(
        [Parameter(Mandatory = $true)][string]$Archive,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $destinationRoot = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            $segments = @($name -split '/' | Where-Object { $_ -ne '' })
            if ([string]::IsNullOrWhiteSpace($name) -or
                $name.StartsWith('/') -or
                $name.Contains(':') -or
                ($segments | Where-Object { $_ -eq '..' })) {
                throw "Unsafe ZIP entry: $($entry.FullName)"
            }
            $destinationPath = [IO.Path]::GetFullPath((Join-Path $Destination $name))
            if (-not $destinationPath.StartsWith(
                $destinationRoot,
                [StringComparison]::OrdinalIgnoreCase
            )) {
                throw "ZIP entry escapes the staging directory: $($entry.FullName)"
            }
        }
    } finally {
        $zip.Dispose()
    }
}

function Test-ApplicationAvailable {
    param([Parameter(Mandatory = $true)][string]$Name)
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    return $null -ne $command -and
        -not ([IO.Path]::GetFullPath($command.Source) -match "(?i)\\WindowsApps\\")
}

$missingDefaultTool = -not $SkipDefaultTools -and (
    -not (Test-ApplicationAvailable -Name "git.exe") -or
    -not (Test-ApplicationAvailable -Name "rg.exe") -or
    -not (Test-ApplicationAvailable -Name "uv.exe")
)
$requiresElevation = -not $SkipPowerShellRuntime -or $missingDefaultTool

if ($requiresElevation -and -not (Test-IsAdministrator)) {
    # Start-Process joins ArgumentList entries into one native command line.
    # Empty values (notably the default InstallDir) and paths containing spaces
    # therefore cannot be forwarded safely as a plain string array. Serialize
    # the child parameters and invoke the elevated copy through EncodedCommand.
    $elevatedParameters = @{
        ScriptPath = $PSCommandPath
        PowerShellVersion = $PowerShellVersion
        NoPathUpdate = [bool]$NoPathUpdate
        SetupSandbox = [bool]$SetupSandbox
        SkipPowerShellRuntime = [bool]$SkipPowerShellRuntime
        SkipDefaultTools = [bool]$SkipDefaultTools
    }
    if (-not [string]::IsNullOrWhiteSpace($InstallDir)) {
        $elevatedParameters["InstallDir"] = $InstallDir
    }
    if (-not [string]::IsNullOrWhiteSpace($PowerShellArchive)) {
        $elevatedParameters["PowerShellArchive"] = $PowerShellArchive
        $elevatedParameters["PowerShellArchiveSha256"] = $PowerShellArchiveSha256
    }

    $elevationLog = Join-Path ([IO.Path]::GetTempPath()) (
        "atelier-install-elevated-{0}.log" -f [Guid]::NewGuid().ToString("N")
    )
    $elevatedParameters["LogPath"] = $elevationLog
    $payloadJson = $elevatedParameters | ConvertTo-Json -Compress
    $payloadBase64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payloadJson))
    $elevatedCommand = @"
`$ErrorActionPreference = "Stop"
`$payloadJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("$payloadBase64"))
`$payload = `$payloadJson | ConvertFrom-Json
`$invokeParameters = @{
    PowerShellVersion = [string]`$payload.PowerShellVersion
}
if (`$payload.PSObject.Properties.Name -contains "InstallDir") {
    `$invokeParameters["InstallDir"] = [string]`$payload.InstallDir
}
if ([bool]`$payload.NoPathUpdate) { `$invokeParameters["NoPathUpdate"] = `$true }
if ([bool]`$payload.SetupSandbox) { `$invokeParameters["SetupSandbox"] = `$true }
if ([bool]`$payload.SkipPowerShellRuntime) { `$invokeParameters["SkipPowerShellRuntime"] = `$true }
if ([bool]`$payload.SkipDefaultTools) { `$invokeParameters["SkipDefaultTools"] = `$true }
if (`$payload.PSObject.Properties.Name -contains "PowerShellArchive") {
    `$invokeParameters["PowerShellArchive"] = [string]`$payload.PowerShellArchive
    `$invokeParameters["PowerShellArchiveSha256"] = [string]`$payload.PowerShellArchiveSha256
}
try {
    & ([string]`$payload.ScriptPath) @invokeParameters *>&1 |
        Out-File -LiteralPath ([string]`$payload.LogPath) -Encoding UTF8
    exit 0
} catch {
    (`$_ | Out-String) | Out-File -LiteralPath ([string]`$payload.LogPath) -Append -Encoding UTF8
    exit 1
}
"@
    $encodedCommand = [Convert]::ToBase64String(
        [Text.Encoding]::Unicode.GetBytes($elevatedCommand)
    )

    Write-Host "Administrator approval is required to install Atelier managed runtimes or default tools."
    try {
        $process = Start-Process -FilePath "powershell.exe" -Verb RunAs -Wait -PassThru -ArgumentList @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", $encodedCommand
        )
    } catch {
        Remove-Item -LiteralPath $elevationLog -Force -ErrorAction SilentlyContinue
        throw
    }

    $elevatedOutput = if (Test-Path -LiteralPath $elevationLog -PathType Leaf) {
        (Get-Content -LiteralPath $elevationLog -Raw).Trim()
    } else {
        ""
    }
    Remove-Item -LiteralPath $elevationLog -Force -ErrorAction SilentlyContinue
    if ($process.ExitCode -ne 0) {
        if ([string]::IsNullOrWhiteSpace($elevatedOutput)) {
            throw "Elevated Atelier installation failed with exit code $($process.ExitCode)"
        }
        throw "Elevated Atelier installation failed with exit code $($process.ExitCode).`nElevated installer output:`n$elevatedOutput"
    }
    if (-not [string]::IsNullOrWhiteSpace($elevatedOutput)) {
        Write-Host $elevatedOutput
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
    $backup = Join-Path $installPath (
        "ate.exe.old.{0}.{1}" -f $PID, [Guid]::NewGuid().ToString("N")
    )
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
            Invoke-DownloadWithRetry -Uri "$releaseBase/$archiveName" -OutFile $temporaryDownload
            Invoke-DownloadWithRetry -Uri "$releaseBase/hashes.sha256" -OutFile $checksumsDownload
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
            Assert-SafeZipArchive -Archive $Archive -Destination $staging
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

function Install-ManagedArchiveTool {
    param(
        [Parameter(Mandatory = $true)][string]$Id,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$ArchiveName,
        [Parameter(Mandatory = $true)][string]$ArchiveUrl,
        [Parameter(Mandatory = $true)][string]$ArchiveSha256,
        [Parameter(Mandatory = $true)][string]$ExecutableName,
        [string]$ExpectedRelativeSuffix = "",
        [string[]]$ProbeArguments = @("--version")
    )

    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) {
        throw "ProgramData is not set; cannot install managed tool $Id"
    }
    if ($ArchiveSha256 -notmatch "(?i)^[0-9a-f]{64}$") {
        throw "Managed tool $Id requires a fixed 64-character SHA-256"
    }

    $toolRoot = Join-Path $env:ProgramData ("Atelier\tools\{0}" -f $Id)
    $versionRoot = Join-Path $toolRoot $Version
    $activeManifest = Join-Path $toolRoot "active.json"
    New-Item -ItemType Directory -Path $toolRoot -Force | Out-Null
    Set-AtelierReadExecuteAcl -Path $toolRoot

    if (Test-Path -LiteralPath $activeManifest -PathType Leaf) {
        try {
            $active = Get-Content -LiteralPath $activeManifest -Raw | ConvertFrom-Json
            $activeExecutable = [IO.Path]::GetFullPath([string]$active.executable)
            if ([string]$active.version -eq $Version -and
                (Test-Path -LiteralPath $activeExecutable -PathType Leaf)) {
                & $activeExecutable @ProbeArguments | Out-Null
                if ($LASTEXITCODE -eq 0) {
                    $activeRoot = Split-Path -Parent $activeExecutable
                    if (($env:Path -split ";") -notcontains $activeRoot) {
                        $env:Path = "$activeRoot;$env:Path"
                    }
                    return $activeExecutable
                }
            }
        } catch {
            # A malformed or stale manifest is repaired by the verified install below.
        }
    }

    $temporaryDownload = Join-Path ([IO.Path]::GetTempPath()) (
        "atelier-{0}-{1}-{2}" -f $Id, $PID, $ArchiveName
    )
    $staging = Join-Path $toolRoot (".staging-{0}-{1}" -f $Version, $PID)
    try {
        Invoke-DownloadWithRetry -Uri $ArchiveUrl -OutFile $temporaryDownload
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $temporaryDownload).Hash
        if ($actualHash -ne $ArchiveSha256.ToUpperInvariant()) {
            throw "Managed tool $Id archive SHA-256 mismatch"
        }

        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Path $staging -Force | Out-Null
        Assert-SafeZipArchive -Archive $temporaryDownload -Destination $staging
        Expand-Archive -LiteralPath $temporaryDownload -DestinationPath $staging -Force
        $reparsePoint = Get-ChildItem -LiteralPath $staging -Recurse -Force | Where-Object {
            $_.Attributes -band [IO.FileAttributes]::ReparsePoint
        } | Select-Object -First 1
        if ($reparsePoint) {
            throw "Managed tool $Id archive contains a reparse point: $($reparsePoint.FullName)"
        }

        $candidates = @(Get-ChildItem -LiteralPath $staging -Recurse -File -Filter $ExecutableName)
        if (-not [string]::IsNullOrWhiteSpace($ExpectedRelativeSuffix)) {
            $normalizedSuffix = $ExpectedRelativeSuffix.Replace('/', '\').TrimStart('\')
            $candidates = @($candidates | Where-Object {
                $relative = $_.FullName.Substring($staging.Length).TrimStart('\')
                $relative.EndsWith($normalizedSuffix, [StringComparison]::OrdinalIgnoreCase)
            })
        }
        if ($candidates.Count -ne 1) {
            throw "Managed tool $Id archive must contain exactly one expected $ExecutableName"
        }
        $relativeExecutable = $candidates[0].FullName.Substring($staging.Length).TrimStart('\')
        & $candidates[0].FullName @ProbeArguments | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "Managed tool $Id failed its startup probe"
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

        Set-AtelierReadExecuteAcl -Path $versionRoot
        foreach ($sandboxAccount in @("AtelierSandbox", "AtelierSandboxNoNet")) {
            if (Get-LocalUser -Name $sandboxAccount -ErrorAction SilentlyContinue) {
                & icacls.exe $versionRoot /grant:r ("{0}:(OI)(CI)RX" -f $sandboxAccount) | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "Failed to grant $sandboxAccount access to managed tool $Id"
                }
            }
        }

        $installedExecutable = Join-Path $versionRoot $relativeExecutable
        $executableRoot = Split-Path -Parent $installedExecutable
        $manifestTemporary = "$activeManifest.new.$PID"
        $manifest = @{
            schema_version = 1
            id = $Id
            version = $Version
            architecture = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
            executable = $installedExecutable
            roots = @($executableRoot)
        } | ConvertTo-Json -Depth 4
        Write-Utf8NoBom -Path $manifestTemporary -Value $manifest
        Move-Item -LiteralPath $manifestTemporary -Destination $activeManifest -Force
        if (($env:Path -split ";") -notcontains $executableRoot) {
            $env:Path = "$executableRoot;$env:Path"
        }
        Write-Host "Managed $Id installed: $installedExecutable"
        return $installedExecutable
    } finally {
        Remove-Item -LiteralPath $temporaryDownload -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-DefaultManagedTools {
    if ([string]::IsNullOrWhiteSpace($env:ProgramData)) {
        throw "ProgramData is not set; cannot install default managed tools"
    }
    $architecture = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }

    if (-not (Test-ApplicationAvailable -Name "git.exe")) {
        $gitVersion = "2.55.0.3"
        $gitArchive = if ($architecture -eq "arm64") {
            "MinGit-2.55.0.3-arm64.zip"
        } else {
            "MinGit-2.55.0.3-64-bit.zip"
        }
        $gitHash = if ($architecture -eq "arm64") {
            "f7748965d5068e81ad93ca1923650db6742d6e22332b1ae7567a841c59f6bde5"
        } else {
            "f48e2d2dc74a24454adc6d8fd0ac25bf9c2386f19cfb06202b9465aaad4f9f05"
        }
        Install-ManagedArchiveTool `
            -Id "git" `
            -Version $gitVersion `
            -ArchiveName $gitArchive `
            -ArchiveUrl "https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.3/$gitArchive" `
            -ArchiveSha256 $gitHash `
            -ExecutableName "git.exe" `
            -ExpectedRelativeSuffix "cmd\git.exe" | Out-Null
    }

    if (-not (Test-ApplicationAvailable -Name "rg.exe")) {
        $rgVersion = "15.2.0"
        $rgArchive = if ($architecture -eq "arm64") {
            "ripgrep-15.2.0-aarch64-pc-windows-msvc.zip"
        } else {
            "ripgrep-15.2.0-x86_64-pc-windows-msvc.zip"
        }
        $rgHash = if ($architecture -eq "arm64") {
            "e4abca10c3a64ebea742667dd7009449d49403db5460dd6873e389fa2945360f"
        } else {
            "71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5"
        }
        Install-ManagedArchiveTool `
            -Id "ripgrep" `
            -Version $rgVersion `
            -ArchiveName $rgArchive `
            -ArchiveUrl "https://github.com/BurntSushi/ripgrep/releases/download/$rgVersion/$rgArchive" `
            -ArchiveSha256 $rgHash `
            -ExecutableName "rg.exe" | Out-Null
    }

    if (-not (Test-ApplicationAvailable -Name "uv.exe")) {
        $uvVersion = "0.12.1"
        $uvArchive = if ($architecture -eq "arm64") {
            "uv-aarch64-pc-windows-msvc.zip"
        } else {
            "uv-x86_64-pc-windows-msvc.zip"
        }
        $uvHash = if ($architecture -eq "arm64") {
            "9bc7c18e616230fa2dc6fb24bc3afde18a95c2b5c9433de747e9502c66041568"
        } else {
            "8fcb0cb46e1229065e344758980924e569bef5882ef45f46fada8fb24e06b74a"
        }
        Install-ManagedArchiveTool `
            -Id "uv" `
            -Version $uvVersion `
            -ArchiveName $uvArchive `
            -ArchiveUrl "https://github.com/astral-sh/uv/releases/download/$uvVersion/$uvArchive" `
            -ArchiveSha256 $uvHash `
            -ExecutableName "uv.exe" | Out-Null
    }
}

function Write-ToolchainRecommendations {
    if (-not (Test-ApplicationAvailable -Name "node.exe")) {
        Write-Host "Node.js is recommended for TypeScript and JavaScript projects; ask the Agent to prepare a managed Node.js toolchain when needed."
    }
    if (-not (Test-ApplicationAvailable -Name "rustup.exe") -and
        -not (Test-ApplicationAvailable -Name "cargo.exe")) {
        Write-Host "Rust is recommended for Rust projects; ask the Agent to prepare a managed rustup toolchain when needed."
    }
    Write-Host "Python runtimes, C/C++, Java, and .NET toolchains can also be prepared as project requirements are detected."
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

    Get-ChildItem -LiteralPath $toolRoot -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $toolManifest = Join-Path $_.FullName "active.json"
        if (Test-Path -LiteralPath $toolManifest -PathType Leaf) {
            try {
                $managedTool = Get-Content -LiteralPath $toolManifest -Raw | ConvertFrom-Json
                foreach ($managedRootValue in @($managedTool.roots)) {
                    $managedRoot = [IO.Path]::GetFullPath([string]$managedRootValue)
                    if (-not ($roots | Where-Object {
                        [string]::Equals($_.path, $managedRoot, [StringComparison]::OrdinalIgnoreCase)
                    })) {
                        $roots += @{ path = $managedRoot; enabled = $true }
                    }
                }
            } catch {
                throw "Invalid managed tool manifest: $toolManifest"
            }
        }
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
}
if (-not $SkipDefaultTools) {
    Install-DefaultManagedTools
}
if (-not $SkipPowerShellRuntime -or $missingDefaultTool) {
    Write-ToolchainRegistry
}
Write-ToolchainRecommendations

if ($SetupSandbox) {
    & $destination sandbox setup
    if ($LASTEXITCODE -ne 0) {
        throw "Atelier was installed, but sandbox setup failed with exit code $LASTEXITCODE"
    }
}

Write-Host "Atelier installed: $destination"
Write-Host "Run 'ate' to configure a Provider and select a model."
