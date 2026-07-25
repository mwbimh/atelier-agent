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
fn windows_installer_is_offline_and_uses_the_adjacent_binary() {
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
    assert!(source.contains("--version"));
    assert!(source.contains("sandbox setup"));
    assert!(!source.contains("Invoke-WebRequest"));
    assert!(!source.contains("ATELIER_RELEASE_BASE_URL"));
}
