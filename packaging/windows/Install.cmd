@echo off
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0Start.ps1" -Install
if errorlevel 1 pause
