@echo off
chcp 65001 >nul
echo ==========================================
echo  LiteLLM Proxy + Web UI 一键启动
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"

REM === 启动 LiteLLM Proxy ===
start "LiteLLM Proxy" cmd /k "cd /d %SCRIPT_DIR% && %SCRIPT_DIR%start.bat"

REM === 启动 Web UI ===
start "LiteLLM Web UI" cmd /k "cd /d %SCRIPT_DIR%web && node server.js"

echo.
echo 两个窗口已启动：
echo   - Proxy:   http://localhost:4000
echo   - Web UI:  http://localhost:8080
echo.
pause
