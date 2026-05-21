@echo off
chcp 65001 >nul
echo ==========================================
echo  LiteLLM Unified Web Proxy
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
start "LiteLLM Web Proxy" cmd /k "cd /d %SCRIPT_DIR%web && node server.js"

echo.
echo 服务已启动：
echo   - Web UI / API: http://localhost:18783
echo.
pause
