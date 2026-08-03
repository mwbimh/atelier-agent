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
fn release_build_pins_the_current_commit_into_every_version_build_script() {
    let release_script = include_str!("../../../tools/build-release.ps1");
    let cli_build_script = include_str!("../build.rs");
    let pager_build_script = include_str!("../../../crates/codegen/atelier-pager/build.rs");

    assert!(release_script.contains("ATELIER_BUILD_COMMIT"));
    assert!(release_script.contains("rev-parse --short HEAD"));
    for source in [cli_build_script, pager_build_script] {
        assert!(source.contains("cargo:rerun-if-env-changed=ATELIER_BUILD_COMMIT"));
        assert!(source.contains("std::env::var(\"ATELIER_BUILD_COMMIT\")"));
    }
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
    assert!(source.contains("ate.exe.old.{0}.{1}"));
    assert!(!source.contains("Join-Path $installPath \"ate.exe.old\""));
    assert!(source.contains("[switch]$NoPathUpdate"));
    assert!(source.contains("[switch]$SetupSandbox"));
    assert!(source.contains("[string]$PowerShellArchive"));
    assert!(source.contains("[string]$PowerShellArchiveSha256"));
    assert!(source.contains("[switch]$SkipDefaultTools"));
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
    assert!(source.contains("Install-DefaultManagedTools"));
    assert!(source.contains("Assert-SafeZipArchive"));
    assert!(source.contains("ripgrep-15.2.0"));
    assert!(source.contains("71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5"));
    assert!(source.contains("uv-x86_64-pc-windows-msvc.zip"));
    assert!(source.contains("8fcb0cb46e1229065e344758980924e569bef5882ef45f46fada8fb24e06b74a"));
    assert!(source.contains("MinGit-2.55.0.3-64-bit.zip"));
    assert!(source.contains("f48e2d2dc74a24454adc6d8fd0ac25bf9c2386f19cfb06202b9465aaad4f9f05"));
    assert!(source.contains("Node.js is recommended"));
    assert!(source.contains("Rust is recommended"));
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
    assert!(source.contains("SkipDefaultTools = [bool]$SkipDefaultTools"));
    assert!(source.contains("SkipPowerShellRuntime = [bool]$SkipPowerShellRuntime"));
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
