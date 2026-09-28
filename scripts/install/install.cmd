@echo off
rem xmux installer for Windows CMD.
rem
rem   curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd && del install.cmd
rem   install.cmd -Version 0.9.6
rem
rem CMD cannot verify a download or edit the user PATH on its own, so this hands
rem the whole install to install.ps1 from the same latest release, through the
rem Windows PowerShell every supported Windows ships. Its options pass through
rem unchanged, and so do the XMUX_* environment variables it reads.
setlocal

where powershell >nul 2>&1
if errorlevel 1 (
    echo install.cmd: Windows PowerShell is required but not on PATH 1>&2
    exit /b 1
)

rem A CMD opened from PowerShell 7 hands its module path down, and Windows
rem PowerShell then fails to load its own built-in modules; with the variable
rem cleared it falls back to its defaults.
set "PSModulePath="

rem TLS 1.2 is forced because an older Windows PowerShell still offers only
rem TLS 1.0 by default, which GitHub refuses.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12;" ^
  "& ([scriptblock]::Create((Invoke-RestMethod -UseBasicParsing https://github.com/zer0ken/xmux/releases/latest/download/install.ps1))) %*"
exit /b %ERRORLEVEL%
