param(
    [Parameter(Position = 0)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($env:ATELIER_RELEASE_BASE_URL)) {
    Write-Error 'ATELIER_RELEASE_BASE_URL must point to the Atelier release directory.'
    exit 1
}

if ([string]::IsNullOrWhiteSpace($env:ATELIER_CHANNEL)) {
    $env:ATELIER_CHANNEL = 'enterprise'
}

$installer = if ([string]::IsNullOrWhiteSpace($PSScriptRoot)) {
    $null
} else {
    Join-Path $PSScriptRoot 'install.ps1'
}
if ($installer -and (Test-Path -LiteralPath $installer -PathType Leaf)) {
    & $installer -Version $Version
    exit $LASTEXITCODE
}

$baseUrl = $env:ATELIER_RELEASE_BASE_URL.TrimEnd('/')
$installerUrl = if ([string]::IsNullOrWhiteSpace($env:ATELIER_INSTALLER_URL)) {
    "$baseUrl/install.ps1"
} else {
    $env:ATELIER_INSTALLER_URL
}

try {
    $source = (Invoke-WebRequest -Uri $installerUrl -UseBasicParsing).Content
    $script = [scriptblock]::Create($source)
} catch {
    Write-Error "Failed to fetch the Atelier installer from ${installerUrl}: $($_.Exception.Message)"
    exit 1
}

& $script -Version $Version
exit $LASTEXITCODE
