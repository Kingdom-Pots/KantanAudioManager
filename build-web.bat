@echo off
rem Builds the browser version into dist-web\site and zips it for itch.io as
rem dist-web\KantanAudioManager-web.zip. Needs the wasm32-unknown-unknown target,
rem wasm-bindgen-cli matching the project's wasm-bindgen version, and Python.
setlocal
cd /d "%~dp0"
call cargo build --release --target wasm32-unknown-unknown --bin kantan-audio-manager
if errorlevel 1 goto failed
for /f "delims=" %%d in ('cargo metadata --format-version 1 --no-deps ^| python -c "import json,sys;print(json.load(sys.stdin)['target_directory'])"') do set TARGET=%%d
if exist dist-web rmdir /s /q dist-web
mkdir dist-web\site
wasm-bindgen --target web --no-typescript --out-dir dist-web\site "%TARGET%\wasm32-unknown-unknown\release\kantan-audio-manager.wasm"
if errorlevel 1 goto failed
copy /y web\index.html dist-web\site\index.html >nul
copy /y assets\logo\exports\kantan_64.png dist-web\site\icon.png >nul
copy /y LICENSE dist-web\site\LICENSE.txt >nul
copy /y THIRD_PARTY_NOTICES.txt dist-web\site\THIRD_PARTY_NOTICES.txt >nul
powershell -NoProfile -Command "Compress-Archive -Path 'dist-web\site\*' -DestinationPath 'dist-web\KantanAudioManager-web.zip' -Force"
if errorlevel 1 goto failed
echo Built dist-web\KantanAudioManager-web.zip
exit /b 0
:failed
echo.
echo The web build failed. See the messages above.
exit /b 1
