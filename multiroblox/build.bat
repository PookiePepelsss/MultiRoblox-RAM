@echo off
title Build
echo Building...
echo.

where node >nul 2>&1
if errorlevel 1 (echo Node.js not found & pause & exit /b 1)

call npm install
if errorlevel 1 (echo Install failed & pause & exit /b 1)

if exist dist rmdir /s /q dist

call npm run build
if errorlevel 1 (echo Build failed & pause & exit /b 1)

echo.
echo Done. Output in dist\
pause
