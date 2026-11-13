@echo off
setlocal

cd /d "%~dp0"
if errorlevel 1 exit /b %errorlevel%

cargo build
if errorlevel 1 exit /b %errorlevel%

git add -A
if errorlevel 1 exit /b %errorlevel%

git commit -m "auto"
if errorlevel 1 exit /b %errorlevel%

git push
exit /b %errorlevel%
