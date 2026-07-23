[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $AteArguments
)

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$requestedProfile = $env:ATELIER_PROFILE
$profiles = if ([string]::IsNullOrWhiteSpace($requestedProfile)) {
    @("release", "release-dist", "debug")
} else {
    @($requestedProfile)
}

foreach ($profile in $profiles) {
    $binaryPath = Join-Path $repositoryRoot (Join-Path "target\$profile" "ate.exe")
    if (Test-Path -LiteralPath $binaryPath -PathType Leaf) {
        & $binaryPath @AteArguments
        exit $LASTEXITCODE
    }
}

Write-Error "找不到 ate.exe。请先运行: cargo build --release -p atelier-pager-bin --bin ate"
exit 1
