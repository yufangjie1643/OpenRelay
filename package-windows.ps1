param(
  [string]$Configuration = "release",
  [switch]$Zip
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Version = (Select-String -Path (Join-Path $Root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches.Groups[1].Value
$PackageName = "OpenRelay-v$Version-windows-x64"
$OutDir = Join-Path $Root "dist\$PackageName"

Push-Location $Root
try {
  cargo build --release
  New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
  Copy-Item -LiteralPath "target\release\openrelay.exe" -Destination (Join-Path $OutDir "openrelay.exe") -Force
  Copy-Item -LiteralPath "public" -Destination $OutDir -Recurse -Force
  Copy-Item -LiteralPath "assets" -Destination $OutDir -Recurse -Force
  Copy-Item -LiteralPath "README.md","config.example.json","install.ps1","uninstall.ps1" -Destination $OutDir -Force

  if ($Zip) {
    $ZipPath = Join-Path $Root "dist\$PackageName.zip"
    if (Test-Path $ZipPath) { Remove-Item -LiteralPath $ZipPath -Force }
    Compress-Archive -LiteralPath (Join-Path $OutDir "*") -DestinationPath $ZipPath
    Write-Host "Package written to $ZipPath"
  } else {
    Write-Host "Package written to $OutDir"
  }
}
finally {
  Pop-Location
}
