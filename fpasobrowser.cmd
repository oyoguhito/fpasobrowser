@echo off
setlocal DisableDelayedExpansion

rem One Windows entry point for both a release archive and a source checkout.
rem A source checkout is identified by its Cargo manifest and checked-in
rem PowerShell launcher. Check it first: an older release fpasobrowser.exe in
rem the checkout root must never mask target\debug or prevent --rebuild from
rem compiling the current source. A release archive contains only this CMD file
rem plus fpasobrowser.exe.
set "FPASOBROWSER_ROOT=%~dp0"

if exist "%FPASOBROWSER_ROOT%Cargo.toml" if exist "%FPASOBROWSER_ROOT%bin\fpasobrowser.ps1" goto run_source
if exist "%FPASOBROWSER_ROOT%fpasobrowser.exe" goto run_release

echo fpasobrowser: fpasobrowser.exe and bin\fpasobrowser.ps1 were not found beside this launcher. 1>&2
exit /b 127

:run_release
"%FPASOBROWSER_ROOT%fpasobrowser.exe" %*
exit /b %ERRORLEVEL%

:run_source
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%FPASOBROWSER_ROOT%bin\fpasobrowser.ps1" %*
exit /b %ERRORLEVEL%
