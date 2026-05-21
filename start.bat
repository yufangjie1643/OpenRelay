@echo off
chcp 65001 >nul
echo ==========================================
echo  LiteLLM Unified Web Proxy
echo  Web UI / API: http://localhost:18783
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
cd /d "%SCRIPT_DIR%web"
node server.js

pause
