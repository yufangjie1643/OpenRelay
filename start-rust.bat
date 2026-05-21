@echo off
chcp 65001 >nul
echo ==========================================
echo  OpenRelay Rust Backend
echo  Personal unified LLM API gateway: http://localhost:18783
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
set "OPENRELAY_ROOT=%SCRIPT_DIR:~0,-1%"
set "OPENRELAY_EXE=%SCRIPT_DIR%openrelay.exe"
set "BUILT_EXE=%SCRIPT_DIR%target\release\openrelay.exe"

if not exist "%OPENRELAY_EXE%" set "OPENRELAY_EXE=%BUILT_EXE%"

if not exist "%OPENRELAY_EXE%" (
  echo Building OpenRelay release executable...
  cd /d "%SCRIPT_DIR%"
  cargo build --release
  if errorlevel 1 (
    echo.
    echo Rust release build failed.
    pause
    exit /b 1
  )
  set "OPENRELAY_EXE=%BUILT_EXE%"
)

cd /d "%SCRIPT_DIR%"
start "" "%OPENRELAY_EXE%"
echo OpenRelay 已在系统托盘启动。

pause
