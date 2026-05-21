# OpenRelay setup script
# Installs the Node.js admin UI / proxy dependencies.

$ErrorActionPreference = "Stop"
$ProjectDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectDir

Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "  OpenRelay Setup" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Write-Error "Node.js not found. Please install Node.js first: https://nodejs.org/"
    exit 1
}

if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    Write-Error "npm not found. Please install Node.js with npm first: https://nodejs.org/"
    exit 1
}

Write-Host "Installing Web UI dependencies..." -ForegroundColor Yellow
Push-Location (Join-Path $ProjectDir "web")
npm install
Pop-Location

Write-Host "==========================================" -ForegroundColor Green
Write-Host "  Setup complete!" -ForegroundColor Green
Write-Host "  Run .\start.bat to start OpenRelay at http://localhost:18783." -ForegroundColor Green
Write-Host "==========================================" -ForegroundColor Green
