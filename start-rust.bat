@echo off
chcp 65001 >nul
echo ==========================================
echo  OpenRelay Rust Backend
echo  Personal unified LLM API gateway: http://localhost:18783
echo ==========================================
echo.

set "SCRIPT_DIR=%~dp0"
set "OPENRELAY_ROOT=%SCRIPT_DIR:~0,-1%"
cd /d "%SCRIPT_DIR%rust-backend"
cargo run

pause
