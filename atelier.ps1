[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $AtelierArguments
)

$ErrorActionPreference = "Stop"

$repositoryRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$requestedProfile = $env:ATELIER_PROFILE

if ([string]::IsNullOrWhiteSpace($requestedProfile)) {
    $profiles = @("release", "release-dist", "debug")
} else {
    $profiles = @($requestedProfile)
}

$binaryPath = $null
$profile = $null
foreach ($candidate in $profiles) {
    $candidatePath = Join-Path $repositoryRoot (Join-Path "target\$candidate" "atelier.exe")
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) {
        $binaryPath = $candidatePath
        $profile = $candidate
        break
    }
}

if ($null -eq $binaryPath) {
    $profileHint = if ([string]::IsNullOrWhiteSpace($requestedProfile)) {
        "release、release-dist 或 debug"
    } else {
        $requestedProfile
    }
    Write-Error @"
找不到 Atelier 可执行文件。已检查 profile: $profileHint。
请先构建，例如：
  cargo build --release -p atelier-pager-bin --bin atelier
"@
    exit 1
}

Write-Verbose "启动 $binaryPath ($profile)"
& $binaryPath @AtelierArguments
exit $LASTEXITCODE
