use std::path::PathBuf;

#[test]
fn release_build_packages_the_windows_installer_script() {
    let build_script = include_str!("../../../tools/build-release.ps1");

    assert!(build_script.contains("install-windows.ps1"));
    assert!(build_script.contains("ate.exe"));
    assert!(build_script.contains("$expectedNames = @(\"ate.exe\", \"install-windows.ps1\")"));
    assert!(!build_script.contains("$outputEntries.Count -ne 1"));
}

#[test]
fn windows_installer_manages_powershell_runtime_and_uses_the_adjacent_binary() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/install-windows.ps1");
    let source = std::fs::read_to_string(&script).unwrap_or_else(|error| {
        panic!(
            "missing Windows release installer {}: {error}",
            script.display()
        )
    });

    assert!(source.contains("Join-Path $PSScriptRoot 'ate.exe'"));
    assert!(source.contains("[switch]$NoPathUpdate"));
    assert!(source.contains("[switch]$SetupSandbox"));
    assert!(source.contains("[string]$PowerShellArchive"));
    assert!(source.contains("[string]$PowerShellArchiveSha256"));
    assert!(source.contains("PowerShell-$PowerShellVersion-win-$architecture.zip"));
    assert!(source.contains("PowerShell version must use numeric major.minor.patch format"));
    assert!(source.contains("Atelier\\runtimes\\powershell"));
    assert!(source.contains("active.json"));
    assert!(source.contains("[Text.UTF8Encoding]::new($false)"));
    assert!(source.contains(".backup-$PID"));
    assert!(source.contains("Get-AuthenticodeSignature"));
    assert!(source.contains("*S-1-5-32-545"));
    assert!(source.contains("$approvedPrefixes"));
    assert!(source.contains("[IO.FileAttributes]::ReparsePoint"));
    assert!(source.contains("Invoke-WebRequest"));
    assert!(source.contains("--version"));
    assert!(source.contains("sandbox setup"));
    assert!(!source.contains("ATELIER_RELEASE_BASE_URL"));
}

#[test]
fn windows_installer_elevation_preserves_optional_arguments_and_child_errors() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/install-windows.ps1");
    let source = std::fs::read_to_string(&script).unwrap_or_else(|error| {
        panic!(
            "missing Windows release installer {}: {error}",
            script.display()
        )
    });

    assert!(source.contains("$elevatedParameters = @{"));
    assert!(source.contains("$elevatedParameters[\"InstallDir\"] = $InstallDir"));
    assert!(source.contains("-EncodedCommand"));
    assert!(source.contains("$elevationLog"));
    assert!(source.contains("Elevated installer output:"));
    assert!(
        !source.contains(
            "\"-InstallDir\", $InstallDir,\n        \"-PowerShellVersion\", $PowerShellVersion"
        ),
        "an empty InstallDir must not consume -PowerShellVersion during the elevated handoff"
    );
}
