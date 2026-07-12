@echo off
title Build
cd /d "%~dp0"
echo Building MultiRoblox (Tauri)...
echo.

where node >nul 2>&1
if errorlevel 1 (echo Node.js not found & pause & exit /b 1)

where cargo >nul 2>&1
if errorlevel 1 (echo Rust toolchain not found - install from https://rustup.rs & pause & exit /b 1)

call npm install
if errorlevel 1 (echo Install failed & pause & exit /b 1)

echo Precompiling native helper...
set "CSC=%WINDIR%\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if not exist "%CSC%" set "CSC=%WINDIR%\Microsoft.NET\Framework\v4.0.30319\csc.exe"
if exist "%CSC%" (
  "%CSC%" /nologo /optimize+ /platform:x64 /target:exe /out:src-tauri\resources\RobloxNative.exe src-tauri\resources\RobloxNative.cs
) else (
  echo csc.exe not found - native helper will compile on first launch instead
)

call npm run tauri build
if errorlevel 1 (echo Build failed & pause & exit /b 1)

echo.
echo Done. Installer in src-tauri\target\release\bundle\nsis\
pause
