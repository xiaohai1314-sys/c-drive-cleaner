@echo off
chcp 65001 >nul
cd /d "%~dp0"

where pythonw >nul 2>&1
if %errorlevel%==0 (
    start "" pythonw "main.py"
    exit /b
)

where python >nul 2>&1
if %errorlevel%==0 (
    start "" python "main.py"
    exit /b
)

echo 未找到 Python，请先安装 Python 3.10 或更高版本，并勾选 "Add Python to PATH"。
pause
