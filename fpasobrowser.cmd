@echo off
setlocal DisableDelayedExpansion

rem One Windows entry point for both a release archive and a source checkout.
rem A release archive keeps this file beside fpasobrowser.exe. A source
rem checkout instead delegates to the checked-in PowerShell launcher, which
rem sets the Servo build prerequisites before invoking Cargo.
set "FPASOBROWSER_ROOT=%~dp0"

if exist "%FPASOBROWSER_ROOT%fpasobrowser.exe" goto run_release
if exist "%FPASOBROWSER_ROOT%bin\fpasobrowser.ps1" goto run_source

echo fpasobrowser: fpasobrowser.exe and bin\fpasobrowser.ps1 were not found beside this launcher. 1>&2
exit /b 127

:run_release
"%FPASOBROWSER_ROOT%fpasobrowser.exe" %*
exit /b %ERRORLEVEL%

:run_source
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%FPASOBROWSER_ROOT%bin\fpasobrowser.ps1" %*
exit /b %ERRORLEVEL%
