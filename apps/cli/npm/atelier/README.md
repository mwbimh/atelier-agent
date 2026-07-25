# Atelier CLI npm packaging scaffold

Atelier has not been published to npm. The `@atelier/atelier` package and its
platform packages are development-only packaging scaffolding in this repository.
No npm installation command is currently available.

The manifests are marked `private: true` to prevent accidental publication.
They must remain private until the package name, ownership, release process,
and supported platform artifacts have been explicitly approved and verified.

## Current installation method

The currently verified distribution target is the Windows x64 release produced
at the repository root:

```powershell
.\tools\build-release.ps1 -CleanOutput
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\release\install-windows.ps1
```

The release contains `ate.exe` and the offline `install-windows.ps1` installer.
See the root [`README.md`](../../../../README.md) for current build and installation
instructions.

## Purpose of this directory

This directory preserves and tests the planned cross-platform package layout:

- one CLI package exposing the future `ate` command;
- one binary package for each supported OS and CPU combination;
- post-install logic that selects the matching platform package;
- assembly scripts for native release artifacts.

Keeping this tooling in the repository does not mean that a package exists on
the npm registry.

## Requirements before publication

Before any npm package is made public:

1. Confirm ownership of the intended npm scope and package names.
2. Build each artifact on its native operating-system runner.
3. Complete sandbox and OS-boundary E2E verification for that platform.
4. Verify package signatures, notices, provenance, and installation behavior.
5. Remove `private: true` only as part of the reviewed publication change.
6. Update public documentation only after the packages are actually available.
