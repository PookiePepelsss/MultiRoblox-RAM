@echo off
setlocal enabledelayedexpansion
title MultiRoblox - Build Portable EXE

echo ============================================
echo   MultiRoblox - Portable EXE Builder
echo ============================================
echo.

:: Check Node.js
where node >nul 2>&1
if errorlevel 1 (
    echo [ERROR] Node.js not found. Install from https://nodejs.org
    pause & exit /b 1
)
for /f "tokens=*" %%v in ('node -v') do set NODE_VER=%%v
echo [OK] Node.js %NODE_VER%

:: Check npm
where npm >nul 2>&1
if errorlevel 1 (
    echo [ERROR] npm not found.
    pause & exit /b 1
)

:: Install dependencies
echo.
echo [1/3] Installing dependencies...
call npm install 2>&1
if errorlevel 1 (
    echo [ERROR] npm install failed.
    pause & exit /b 1
)
echo [OK] Dependencies installed.

:: Clean old dist
echo.
echo [2/3] Cleaning previous build...
if exist dist (
    rmdir /s /q dist
    echo [OK] Old dist removed.
) else (
    echo [OK] No previous build to clean.
)

:: Build
echo.
echo [3/3] Building portable EXE (this may take a minute)...
call npm run build 2>&1
if errorlevel 1 (
    echo [ERROR] Build failed. See output above.
    pause & exit /b 1
)

:: Report output
echo.
echo ============================================
echo   BUILD COMPLETE
echo ============================================
if exist dist\MultiRoblox-portable.exe (
    for %%F in (dist\MultiRoblox-portable.exe) do set SIZE=%%~zF
    set /a SIZE_MB=!SIZE! / 1048576
    echo   Output : dist\MultiRoblox-portable.exe
    echo   Size   : ~!SIZE_MB! MB
) else (
    echo   Output folder: dist\
    dir dist /b 2>nul
)
echo.
echo Run the .exe directly - no install needed.
echo ============================================
echo.
pause
