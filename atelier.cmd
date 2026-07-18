@echo off
setlocal

set "REPOSITORY_ROOT=%~dp0"
set "PROFILE=%ATELIER_PROFILE%"

if not defined PROFILE set "PROFILE=debug"
set "ATELIER_BINARY=%REPOSITORY_ROOT%target\%PROFILE%\atelier.exe"

if not exist "%ATELIER_BINARY%" (
    if defined ATELIER_PROFILE goto missing_binary

    for %%P in (release release-dist) do (
        if exist "%REPOSITORY_ROOT%target\%%P\atelier.exe" (
            set "PROFILE=%%P"
            set "ATELIER_BINARY=%REPOSITORY_ROOT%target\%%P\atelier.exe"
            goto binary_found
        )
    )
    goto missing_binary
)

:binary_found
set "ARTIFACT_DIRECTORY=%REPOSITORY_ROOT%target\%PROFILE%"

if not exist "%ARTIFACT_DIRECTORY%\atelier-command-runner.exe" (
    echo Warning: atelier-command-runner.exe not found; Windows sandbox command execution may be unavailable. 1>&2
)

if not exist "%ARTIFACT_DIRECTORY%\atelier-workspace-worker.exe" (
    echo Warning: atelier-workspace-worker.exe not found; Workspace Worker features may be unavailable. 1>&2
)

"%ATELIER_BINARY%" %*
set "EXIT_CODE=%ERRORLEVEL%"
endlocal & exit /b %EXIT_CODE%

:missing_binary
echo Atelier executable not found for profile "%PROFILE%". 1>&2
echo Build it first: cargo build --offline -p atelier-pager-bin --bin atelier 1>&2
endlocal & exit /b 1
