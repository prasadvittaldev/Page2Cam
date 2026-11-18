# Page2Cam

**Page2Cam is a project by Prasad Vittaldev.**
Connect with him on LinkedIn: <https://www.linkedin.com/in/prasadvittaldev/>

---

## What is Page2Cam?

Page2Cam turns any website into a camera.

When you are in a video call on Teams, Zoom or Google Meet, the other people normally see you through your webcam. With Page2Cam, they can see a **website** instead, such as a dashboard, a slide show, a map, a news page or anything else you can open in a browser. You can also put **yourself** on top of that website, so people see both you and the page at the same time.

Page2Cam can also send the same picture, with sound, **live to YouTube**.

You control everything from a simple control page on your own computer. You can change the website, click, scroll and type in it while it is live, and the people watching only see the website, never your controls.

### What you can do with it

- **Show a website as your camera** in Teams, Zoom, Google Meet or any other app that uses a webcam.
- **Go live on YouTube** with the website and its sound.
- **Put yourself on the page** in one of three ways:
  - **Box**: your webcam in a small box that you can move and resize anywhere on the page.
  - **Cut-out small**: just you, with your room removed, standing at the bottom of the page.
  - **Cut-out full**: like a Teams background, except the background is the live website.
- **Choose the cut-out quality**:
  - **Quality** gives cleaner edges around your hair.
  - **Fast** runs smoothly on any computer.

  If your computer can't keep up with Quality, Page2Cam switches to Fast by itself so your video stays smooth.
- **Choose the sound for YouTube**: the website's sound, your microphone, or both, each with its own volume.
- **Use the website live**: click, scroll and type on it from the control page while people are watching.

### What you need

- A computer with **Windows 11**. Windows 10 and Mac are not supported.
- A webcam, if you want to appear on the page. It is optional.
- An internet connection.
- A YouTube account, only if you want to stream to YouTube.

---

## How to install (step by step)

You only need to do steps 1 to 6 once.

1. **Download Page2Cam.**
   Go to the [Releases page](https://github.com/prasadvittaldev/Page2Cam/releases/latest) and download the file called **`Page2Cam-windows-x64.zip`**. It is about 300 MB.

2. **Unblock the download.**
   Find the downloaded zip file (usually in your *Downloads* folder). Right-click it and choose **Properties**. At the bottom of the window, tick **Unblock**, then click **OK**.
   *Windows marks files from the internet as blocked, and a blocked camera driver will not work.*

3. **Unzip it.**
   Right-click the zip file, choose **Extract All…**, and extract it to a simple folder such as `C:\Page2Cam`.

4. **Install the Page2Cam camera.**
   Open the folder. Right-click **`install-camera.cmd`** and choose **Run as administrator**. Click **Yes** when Windows asks for permission.
   A black window appears and finishes with **"Done. The Page2Cam camera is installed."** Press any key to close it.
   *This adds "Page2Cam" to your computer's list of cameras. It also installs a free Microsoft component that the cut-out feature needs.*

5. **Start Page2Cam.**
   Double-click **`page2cam.exe`**.
   If Windows shows **"Windows protected your PC"**, click **More info** and then **Run anyway**. This message appears because this early version is not signed with a paid certificate.
   A black window opens. Leave it open, because closing it closes Page2Cam. A moment later, your web browser opens the **Page2Cam control page**.

6. **Check that it works.**
   On the control page you should see a live picture of the website.

---

## How to use it

### Choose the website
Type a web address at the top of the control page and press **Go**.
You can click, scroll and type directly in the picture to use the page.

### Use it in Teams, Zoom or Google Meet
1. Make sure Page2Cam is running.
2. In your call app, open the **camera settings** and choose **Page2Cam** as the camera.
3. If you don't see Page2Cam in the list, close the call app completely and open it again. For Google Meet, reload the page.

### Put yourself on the page
In the **Webcam overlay** panel:
- Tick **Show webcam**.
- Choose **Box**, **Cut-out small** or **Cut-out full**.
- Drag the dashed box in the picture to move yourself, and drag its corner to resize. In *Cut-out full*, drag the **Move** handle sideways.
- For the cut-out styles, choose **Quality** or **Fast**. The line underneath shows how well it is running.

Page2Cam remembers your choices for next time.

### Go live on YouTube
1. In **YouTube Studio**, click **Go live** and open the **Stream** tab.
2. Copy your **Stream key**.
3. On the Page2Cam control page, paste it into the **YouTube Live** panel.
   Tick **Remember key on this PC** if you don't want to paste it again next time.
4. Click **Go live**. When you are finished, click **End stream**.

### Choose the sound for YouTube
In the **Stream sound** panel, turn **Page sound** and **Microphone** on or off, and set their volumes.
*The website's sound only goes to YouTube. It is not sent into Teams or Zoom calls.*

### Close Page2Cam
Close the black Page2Cam window, or press **Ctrl + C** inside it.

---

## If something goes wrong

The coloured dots at the bottom of the control page show the state of each part: virtual camera, webcam, stream and microphone. Hover over a dot to read the details.

| What you see | What to do |
|---|---|
| "The Page2Cam camera is not installed" | Close Page2Cam, run `install-camera.cmd` **as administrator**, then start Page2Cam again. |
| Page2Cam doesn't appear as a camera in Teams or Meet | Start Page2Cam first, then fully close and reopen the call app, or reload the Meet page. |
| The camera shows a dark picture with a moving light band | The camera is installed but Page2Cam isn't running. Start `page2cam.exe`. |
| "cut-out unavailable" | Run `install-camera.cmd` as administrator again. Keep all the files that came in the zip in the same folder. |
| "Quality was too slow on this PC, switched to Fast" | This is normal on slower computers. Page2Cam switched to keep your video smooth. You can click **Quality** to try again. |
| "ffmpeg was not found" | Keep `ffmpeg.exe` in the Page2Cam folder. |

If you need help, the file **`page2cam.log`** in the Page2Cam folder records what happened. The camera's own record is in `C:\ProgramData\Page2Cam\vcam.log`.

---

## Your privacy

- Page2Cam runs entirely on your own computer.
- The control page only works from your own computer. Other computers and other websites cannot control it.
- Your YouTube stream key is never written to the log. If you tick *Remember key*, it is kept in Windows' own secure password store.
- The cut-out (background removal) happens on your computer. Your video is not sent anywhere to be processed.

---

## How to remove Page2Cam

1. Close Page2Cam.
2. Right-click **`uninstall-camera.cmd`** and choose **Run as administrator**.
3. Delete the Page2Cam folder.

---

## Good to know

- This is an **early version**. Please report anything that doesn't work as expected.
- Page2Cam works on its own and doesn't need OBS or any other streaming program.
- There is also a Linux version for people who build it from the source code. The developer instructions are in [`CLAUDE.md`](CLAUDE.md).

---

## Credits and licence

Page2Cam is free software, released under the **GNU General Public License, version 3 or later** (see [`LICENSE`](LICENSE)).

It is built with these projects (full details in [`packaging/THIRD_PARTY.txt`](packaging/THIRD_PARTY.txt)):
- **Chromium Embedded Framework**: shows the website.
- **FFmpeg**: sends the stream to YouTube.
- **Robust Video Matting** and **Google MediaPipe**: remove the background behind you.
- **Microsoft ONNX Runtime and DirectML**: run the background removal.
- **VCamSample** by Simon Mourier: the basis of the Page2Cam camera.

---

**Page2Cam is a project by Prasad Vittaldev.**
LinkedIn: <https://www.linkedin.com/in/prasadvittaldev/>
