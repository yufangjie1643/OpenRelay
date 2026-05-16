@echo off
chcp 65001 >nul
echo ==========================================
echo  LiteLLM Web UI 启动
echo  地址: http://localhost:8080
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
cd /d "%SCRIPT_DIR%web"
node server.js

pause
