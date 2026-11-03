@echo off
setlocal
set "AGENTNOTIFY_BUILD_ROOT=%~dp0"
set "BUILD_PWSH=%AGENTNOTIFY_PWSH%"
if defined BUILD_PWSH goto run
for /f "usebackq delims=" %%P in (`powershell.exe -NoProfile -Command "$p=Join-Path $env:AGENTNOTIFY_BUILD_ROOT '.build.local.json'; try{if(Test-Path -LiteralPath $p){(Get-Content -LiteralPath $p -Raw | ConvertFrom-Json -ErrorAction Stop).PwshPath}}catch{'__INVALID_BUILD_LOCAL_JSON__'}"`) do if not defined BUILD_PWSH set "BUILD_PWSH=%%P"
if defined BUILD_PWSH goto run
for /f "delims=" %%P in ('where pwsh.exe 2^>nul') do if not defined BUILD_PWSH set "BUILD_PWSH=%%P"
if defined BUILD_PWSH goto run
if exist "%ProgramFiles%\PowerShell\7\pwsh.exe" set "BUILD_PWSH=%ProgramFiles%\PowerShell\7\pwsh.exe"
if defined BUILD_PWSH goto run
echo PowerShell 7 is missing. Install it or set AGENTNOTIFY_PWSH to pwsh.exe.
echo See docs\release\build-from-source.md. Nothing was built.
set "BUILD_EXIT=2"
goto done
:run
if not exist "%BUILD_PWSH%" (
  echo Configured PowerShell path is invalid. Check AGENTNOTIFY_PWSH and .build.local.json.
  set "BUILD_EXIT=2"
  goto done
)
"%BUILD_PWSH%" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build-package.ps1" %*
set "BUILD_EXIT=%ERRORLEVEL%"
:done
echo.
echo Build exit code: %BUILD_EXIT%
if not "%AGENTNOTIFY_NO_PAUSE%"=="1" pause
exit /b %BUILD_EXIT%
