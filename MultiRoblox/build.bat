@echo off
title Build
cd /d "%~dp0"
echo Building MultiRoblox executable...
echo.

where cargo >nul 2>&1
if errorlevel 1 (echo Rust toolchain not found - install from https://rustup.rs & pause & exit /b 1)

echo Precompiling native helper...
set "CSC=%WINDIR%\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if not exist "%CSC%" set "CSC=%WINDIR%\Microsoft.NET\Framework\v4.0.30319\csc.exe"
if exist "%CSC%" (
  "%CSC%" /nologo /optimize+ /platform:x64 /target:exe /r:System.Drawing.dll /out:src-tauri\resources\RobloxNative.exe src-tauri\resources\RobloxNative.cs
) else (
  echo csc.exe not found - native helper will compile on first launch instead
)

cargo build --release --manifest-path src-tauri\Cargo.toml --bin MultiRoblox
if errorlevel 1 (echo Build failed & pause & exit /b 1)

if not exist dist mkdir dist
copy /y "src-tauri\target\release\MultiRoblox.exe" "dist\MultiRoblox.exe" >nul
if errorlevel 1 (echo Could not copy MultiRoblox.exe & pause & exit /b 1)

echo.
echo Done. Executable: dist\MultiRoblox.exe
pause
