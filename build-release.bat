@echo off
rem Builds the optimized app and copies it to dist\KantanAudioManager.exe.
setlocal
cd /d "%~dp0"
call cargo build --release --bin kantan-audio-manager
if errorlevel 1 (
    echo.
    echo The build failed. See the messages above.
    pause
    exit /b 1
)
for /f "delims=" %%d in ('cargo metadata --format-version 1 --no-deps ^| python -c "import json,sys;print(json.load(sys.stdin)['target_directory'])"') do set TARGET=%%d
if not exist dist mkdir dist
copy /y "%TARGET%\release\kantan-audio-manager.exe" dist\KantanAudioManager.exe >nul
copy /y THIRD_PARTY_NOTICES.txt dist\THIRD_PARTY_NOTICES.txt >nul
copy /y LICENSE dist\LICENSE.txt >nul
echo Built dist\KantanAudioManager.exe
