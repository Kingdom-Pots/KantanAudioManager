@echo off
rem Builds Kantan Audio Manager and opens it. The first build takes several
rem minutes; later ones only rebuild what changed. Files passed to this script,
rem or dropped onto it, are imported on startup.
setlocal
cd /d "%~dp0"
title Kantan Audio Manager

where cargo >nul 2>nul
if errorlevel 1 (
    echo Rust is not installed, or cargo is not on your PATH.
    echo Get it from https://rustup.rs
    pause
    exit /b 1
)

call cargo run -- %*
if errorlevel 1 (
    echo.
    echo The build failed or the app exited with an error. See the messages above.
    pause
    exit /b 1
)
