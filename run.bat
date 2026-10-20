@echo off
setlocal

cd /d "%~dp0"
if errorlevel 1 exit /b %errorlevel%

cargo run -- %*
exit /b %errorlevel%
