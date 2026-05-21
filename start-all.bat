@echo off
chcp 65001 >nul
echo ==========================================
echo  OpenRelay
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
start "OpenRelay" cmd /k "cd /d %SCRIPT_DIR%web && node server.js"

echo.
echo 服务已启动：
echo   - OpenRelay: http://localhost:18783
echo.
pause
