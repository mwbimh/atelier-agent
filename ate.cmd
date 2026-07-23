@echo off
setlocal
set "REPOSITORY_ROOT=%~dp0"
set "PROFILE=%ATELIER_PROFILE%"
if not defined PROFILE set "PROFILE=release"
set "ATE_BINARY=%REPOSITORY_ROOT%target\%PROFILE%\ate.exe"

if not exist "%ATE_BINARY%" (
    echo ate.exe not found. Build it first: cargo build --release -p atelier-pager-bin --bin ate 1>&2
    endlocal & exit /b 1
)

"%ATE_BINARY%" %*
set "EXIT_CODE=%ERRORLEVEL%"
endlocal & exit /b %EXIT_CODE%
