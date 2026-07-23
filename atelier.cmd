@echo off
setlocal

set "REPOSITORY_ROOT=%~dp0"
set "PROFILE=%ATELIER_PROFILE%"

if not defined PROFILE set "PROFILE=release"
set "ATELIER_BINARY=%REPOSITORY_ROOT%target\%PROFILE%\atelier.exe"

if not exist "%ATELIER_BINARY%" (
    if defined ATELIER_PROFILE goto missing_binary

    for %%P in (release release-dist debug) do (
        if exist "%REPOSITORY_ROOT%target\%%P\atelier.exe" (
            set "PROFILE=%%P"
            set "ATELIER_BINARY=%REPOSITORY_ROOT%target\%%P\atelier.exe"
            goto binary_found
        )
    )
    goto missing_binary
)

:binary_found
"%ATELIER_BINARY%" %*
set "EXIT_CODE=%ERRORLEVEL%"
endlocal & exit /b %EXIT_CODE%

:missing_binary
echo Atelier executable not found for profile "%PROFILE%". 1>&2
echo Build it first: cargo build --release -p atelier-pager-bin --bin atelier 1>&2
endlocal & exit /b 1
