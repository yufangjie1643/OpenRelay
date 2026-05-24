param(
  [string]$InstallDir = "$env:LOCALAPPDATA\OpenRelay",
  [switch]$RemoveData
)

$ErrorActionPreference = "Stop"
$StartupCmd = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\OpenRelay.cmd"

if (Test-Path $StartupCmd) {
  Remove-Item -LiteralPath $StartupCmd -Force
}

if (Test-Path $InstallDir) {
  Remove-Item -LiteralPath $InstallDir -Recurse -Force
}

if ($RemoveData) {
  $DataDir = Join-Path $env:USERPROFILE ".openrelay"
  if (Test-Path $DataDir) {
    Remove-Item -LiteralPath $DataDir -Recurse -Force
  }
}

Write-Host "OpenRelay uninstalled."
