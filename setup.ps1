# LiteLLM Proxy Setup Script
# Run this first to create the local Python virtual environment

$ErrorActionPreference = "Stop"
$ProjectDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectDir

Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "  LiteLLM Proxy Setup" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

# Check uv
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Write-Error "uv not found. Please install uv first: https://github.com/astral-sh/uv"
    exit 1
}

# Create venv
Write-Host "Creating virtual environment..." -ForegroundColor Yellow
uv venv .venv

# Install litellm with proxy extras
Write-Host "Installing LiteLLM [proxy]..." -ForegroundColor Yellow
uv pip install "litellm[proxy]" --python .venv\Scripts\python.exe

Write-Host "==========================================" -ForegroundColor Green
Write-Host "  Setup complete!" -ForegroundColor Green
Write-Host "  Run .\start.bat to start the proxy." -ForegroundColor Green
Write-Host "==========================================" -ForegroundColor Green
