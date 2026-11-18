@echo off
setlocal
rem Installs the "Page2Cam" virtual camera driver (needs administrator rights once).

net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Asking Windows for administrator permission...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)

set "SRC=%~dp0p2c_vcam_win_source.dll"
set "DEST=%ProgramFiles%\Page2Cam"
if not exist "%SRC%" (
  echo Could not find p2c_vcam_win_source.dll next to this script.
  pause
  exit /b 1
)

echo Stopping the Windows camera service so the driver can be updated...
net stop FrameServerMonitor /y >nul 2>&1
net stop FrameServer /y >nul 2>&1

rem The camera service cannot read files inside your user folder, so the
rem driver is copied to Program Files and registered from there.
mkdir "%DEST%" 2>nul
if exist "%DEST%\p2c_vcam_win_source.dll" regsvr32 /s /u "%DEST%\p2c_vcam_win_source.dll"
copy /y "%SRC%" "%DEST%\" >nul
if errorlevel 1 (
  echo Copying the driver failed. Close Page2Cam and every app using a camera, then run this again.
  pause
  exit /b 1
)
powershell -NoProfile -Command "Unblock-File -Path '%DEST%\p2c_vcam_win_source.dll'" >nul 2>&1

rem Folder for the driver's log file, writable by the camera service.
mkdir "%ProgramData%\Page2Cam" 2>nul
icacls "%ProgramData%\Page2Cam" /grant *S-1-5-19:(OI)(CI)M *S-1-5-32-545:(OI)(CI)M >nul

rem The cut-out (background removal) needs the Microsoft Visual C++ runtime.
if exist "%~dp0vc_redist.x64.exe" (
  echo Installing the Microsoft Visual C++ runtime ^(needed for the cut-out^)...
  "%~dp0vc_redist.x64.exe" /install /quiet /norestart
)

regsvr32 /s "%DEST%\p2c_vcam_win_source.dll"
if errorlevel 1 (
  echo Registering the camera driver failed.
  pause
  exit /b 1
)

echo.
echo Done. The Page2Cam camera is installed.
echo Next: double-click page2cam.exe.
echo.
pause
