@echo off
chcp 65001 >nul
echo ==========================================
echo  LiteLLM Unified Proxy
echo  Endpoint: http://localhost:4000
echo  Master Key: sk-litellm-master-key
echo ==========================================
echo.

REM === Set your actual API keys here ===
set OPENAI_API_KEY=your-openai-key-here
set ANTHROPIC_API_KEY=your-anthropic-key-here
set GEMINI_API_KEY=your-gemini-key-here
set MOONSHOT_API_KEY=your-moonshot-key-here
set DEEPSEEK_API_KEY=your-deepseek-key-here

REM =====================================

set "SCRIPT_DIR=%~dp0"
"%SCRIPT_DIR%.venv\Scripts\litellm.exe" --config "%SCRIPT_DIR%litellm-config.yaml" --port 4000

pause
