@echo off
chcp 65001 >nul
echo ==========================================
echo  Build OpenRelay
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
set "BUILT_EXE=%SCRIPT_DIR%target\release\openrelay.exe"

cd /d "%SCRIPT_DIR%"
cargo build --release
if errorlevel 1 (
  echo.
  echo Build failed.
  exit /b 1
)

echo.
echo Build complete:
echo   %BUILT_EXE%
