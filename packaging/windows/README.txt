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
   - Pick how you appear: Box, Cut-out small or Cut-out full (see below).
3. In Teams/Zoom: open camera settings and pick "Page2Cam".
4. For YouTube Live: in YouTube Studio > Go live > Stream, copy the
   "Stream key", paste it in the YouTube Live panel, then click "Go live".
   Sound: page sound is on by default; tick Microphone to add your voice.


CUT-OUT (REMOVE YOUR ROOM)
--------------------------
Under "Webcam overlay" choose a style:
  Box            - your webcam in a rectangle, room and all.
  Cut-out small  - just you, room removed, standing on the bottom edge of
                   the page. Drag sideways to move, drag the corner to resize.
  Cut-out full   - like a Teams background: the page replaces your room.
                   Drag the "Move" handle sideways to move yourself.
Then choose the cut-out quality:
  Quality - cleaner edges around hair; needs a faster PC (uses your graphics
            chip if it can).
  Fast    - smooth on any PC; rougher edges.
The line under the buttons shows what is running, e.g.
"Quality · GPU (DirectML) · 28 masks/s". If it says "Quality was too slow on
this PC, switched to Fast", Page2Cam switched by itself to keep the video
smooth; you can click Quality again to retry. Your choices are saved in
page2cam.toml.


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
[ ] Cut-out small and Cut-out full: is your room gone? Are the edges around
    your hair and shoulders clean? Tell me what the status line under the
    Quality/Fast buttons says (masks/s, GPU or CPU).
[ ] Does your voice line up with your lips on the YouTube stream?
[ ] In page2cam.log, what does the "webcam measured ... fps" line say?


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
  "cut-out unavailable: ..."              -> onnxruntime.dll, DirectML.dll and the
                                             models folder must stay in this folder.


SETTINGS
--------
Edit page2cam.toml in this folder (Notepad is fine), then restart Page2Cam.


UNINSTALL
---------
Right-click uninstall-camera.cmd > Run as administrator, then delete this folder.


Page2Cam is free software under the GNU GPL v3 or later (LICENSE.txt).
Included third-party software is listed in THIRD_PARTY.txt (Robust Video
Matting, MediaPipe, ONNX Runtime, DirectML, Chromium Embedded Framework,
FFmpeg, VCamSample).
