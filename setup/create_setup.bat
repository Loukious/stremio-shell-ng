@echo off
setlocal
set "mypath=%~dp0"

pushd "%mypath%.."

set "TARGET=x86_64-pc-windows-msvc"
set "EXE=target\%TARGET%\release\stremio-shell-ng.exe"

:: Compile the main executable
if not exist "%EXE%" (
    cargo build --release --target %TARGET%
) else (
    echo Main executable is already built: %EXE%
)

:: Compile the installer
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "%mypath%Stremio.iss"

popd
endlocal
