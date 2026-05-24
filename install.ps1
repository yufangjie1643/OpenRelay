param(
  [string]$InstallDir = "$env:LOCALAPPDATA\OpenRelay",
  [switch]$Startup
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Exe = Join-Path $ScriptDir "openrelay.exe"

if (-not (Test-Path $Exe)) {
  $ReleaseExe = Join-Path $ScriptDir "target\release\openrelay.exe"
  if (Test-Path $ReleaseExe) {
    $Exe = $ReleaseExe
  } else {
    throw "openrelay.exe was not found. Run package-windows.ps1 or cargo build --release first."
  }
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item -LiteralPath $Exe -Destination (Join-Path $InstallDir "openrelay.exe") -Force
Copy-Item -LiteralPath (Join-Path $ScriptDir "public") -Destination $InstallDir -Recurse -Force
Copy-Item -LiteralPath (Join-Path $ScriptDir "assets") -Destination $InstallDir -Recurse -Force

if ($Startup) {
  $StartupDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
  New-Item -ItemType Directory -Force -Path $StartupDir | Out-Null
  $StartupCmd = Join-Path $StartupDir "OpenRelay.cmd"
  "@echo off`r`nstart ""OpenRelay"" ""$InstallDir\openrelay.exe""" | Set-Content -LiteralPath $StartupCmd -Encoding ASCII
}

Write-Host "OpenRelay installed to $InstallDir"
Write-Host "Run: $InstallDir\openrelay.exe"
