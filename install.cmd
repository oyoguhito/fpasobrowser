@echo off
setlocal DisableDelayedExpansion

rem Install Windows source-build prerequisites and build fpasobrowser. The
rem PowerShell implementation keeps this CMD entry point usable from both cmd
rem.exe and PowerShell without requiring a script execution-policy change.
set "FPASOBROWSER_INSTALL_ARGS="

:parse_args
if "%~1"=="" goto run
if /I "%~1"=="--build-only" (
  set "FPASOBROWSER_INSTALL_ARGS=%FPASOBROWSER_INSTALL_ARGS% -BuildOnly"
) else if /I "%~1"=="--no-build" (
  set "FPASOBROWSER_INSTALL_ARGS=%FPASOBROWSER_INSTALL_ARGS% -NoBuild"
) else (
  set "FPASOBROWSER_INSTALL_ARGS=%FPASOBROWSER_INSTALL_ARGS% %~1"
)
shift
goto parse_args

:run
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %FPASOBROWSER_INSTALL_ARGS%
exit /b %ERRORLEVEL%
