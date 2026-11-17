PAGE2CAM FOR WINDOWS 11 - TEST BUILD
====================================

Page2Cam shows a website as a webcam (for Teams, Zoom, Meet...) and/or
streams it to YouTube Live, with your real webcam in a small window on top.


FIRST-TIME SETUP (once per PC)
------------------------------
1. Before unzipping: right-click the downloaded .zip, choose Properties,
   tick "Unblock" at the bottom, click OK. (Otherwise Windows may block
   the camera driver.)
2. Unzip to a normal folder, for example C:\Page2Cam.
3. Right-click install-camera.cmd and choose "Run as administrator".
   It should end with "Done. The Page2Cam camera is installed."


EVERY TIME
----------
1. Double-click page2cam.exe.
   - If Windows says "Windows protected your PC": click "More info",
     then "Run anyway". (This test build is not code-signed.)
   - A black console window opens; leave it open. Closing it quits Page2Cam.
   - Your browser opens the Page2Cam control page.
2. On the control page:
   - Type a website address at the top and press Go.
   - Click, scroll and type directly in the big picture to use the page.
   - Drag the dashed box to move your webcam; drag its corner to resize.
3. In Teams/Zoom: open camera settings and pick "Page2Cam".
4. For YouTube Live: in YouTube Studio > Go live > Stream, copy the
   "Stream key", paste it in the YouTube Live panel, then click "Go live".
   Sound: page sound is on by default; tick Microphone to add your voice.


THINGS TO TRY AND REPORT BACK
-----------------------------
[ ] The control page opens and shows the website.
[ ] Clicking/scrolling/typing in the picture works.
[ ] Your webcam appears in the corner and can be moved.
[ ] Windows Camera app: pick "Page2Cam" (switch camera button) - picture OK?
[ ] Teams: pick "Page2Cam". Do OTHER PEOPLE in the call see it, not just you?
    (Known risk: some virtual cameras only show in your own preview.
     If so, try: start Teams first, then Page2Cam.)
[ ] YouTube Live: does the stream show the page with sound?


IF SOMETHING GOES WRONG
-----------------------
Please send these two files:
  - page2cam.log              (in this folder)
  - C:\ProgramData\Page2Cam\vcam.log   (the camera driver's log)
Also tell me what the coloured dots at the bottom of the control page say
(hover over them for details).

Common messages:
  "the Page2Cam camera is not installed"  -> run install-camera.cmd as administrator.
  Camera shows a dark picture with a moving light band
                                          -> the driver works but page2cam.exe is
                                             not running (start it).
  "ffmpeg was not found"                  -> ffmpeg.exe must stay in this folder.


SETTINGS
--------
Edit page2cam.toml in this folder (Notepad is fine), then restart Page2Cam.


UNINSTALL
---------
Right-click uninstall-camera.cmd > Run as administrator, then delete this folder.


Included third-party software: Chromium Embedded Framework (see CREDITS.html),
FFmpeg (GPL; https://ffmpeg.org). The camera driver is based on
smourier/VCamSample (MIT).
