@echo off
chcp 65001 >nul
echo ==========================================
echo  OpenRelay Rust Backend
echo  Personal unified LLM API gateway: http://localhost:18783
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
set "OPENRELAY_ROOT=%SCRIPT_DIR:~0,-1%"
set "OPENRELAY_EXE=%SCRIPT_DIR%rust-backend\target\release\openrelay.exe"

if not exist "%OPENRELAY_EXE%" (
  echo Building Rust release executable...
  cd /d "%SCRIPT_DIR%rust-backend"
  cargo build --release
  if errorlevel 1 (
    echo.
    echo Rust release build failed.
    pause
    exit /b 1
  )
)

cd /d "%SCRIPT_DIR%"
start "" "%OPENRELAY_EXE%"
echo OpenRelay 已在系统托盘启动。

pause
