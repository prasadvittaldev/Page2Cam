@echo off
setlocal
rem Removes the "Page2Cam" virtual camera driver.

net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Asking Windows for administrator permission...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)

set "DEST=%ProgramFiles%\Page2Cam"
if exist "%DEST%\p2c_vcam_win_source.dll" regsvr32 /s /u "%DEST%\p2c_vcam_win_source.dll"
net stop FrameServerMonitor /y >nul 2>&1
net stop FrameServer /y >nul 2>&1
rmdir /s /q "%DEST%" 2>nul

echo The Page2Cam camera driver was removed.
pause
