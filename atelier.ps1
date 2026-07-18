[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $AtelierArguments
)

$ErrorActionPreference = "Stop"

$repositoryRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$requestedProfile = $env:ATELIER_PROFILE

if ([string]::IsNullOrWhiteSpace($requestedProfile)) {
    $profiles = @("debug", "release", "release-dist")
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
        "debug、release 或 release-dist"
    } else {
        $requestedProfile
    }
    Write-Error @"
找不到 Atelier 可执行文件。已检查 profile: $profileHint。
请先构建，例如：
  cargo build --offline -p atelier-pager-bin --bin atelier
"@
    exit 1
}

$artifactDirectory = Split-Path -Parent $binaryPath
$commandRunnerPath = Join-Path $artifactDirectory "atelier-command-runner.exe"
$workspaceWorkerPath = Join-Path $artifactDirectory "atelier-workspace-worker.exe"

if (-not (Test-Path -LiteralPath $commandRunnerPath -PathType Leaf)) {
    Write-Warning "未找到 $commandRunnerPath；Windows sandbox 的命令执行可能不可用。"
}

if (-not (Test-Path -LiteralPath $workspaceWorkerPath -PathType Leaf)) {
    Write-Warning "未找到 $workspaceWorkerPath；Workspace Worker 功能可能不可用。"
}

Write-Verbose "启动 $binaryPath ($profile)"
& $binaryPath @AtelierArguments
exit $LASTEXITCODE
