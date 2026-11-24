<p align="center"><img src="assets/logo.png" alt="Page2Cam" width="560"></p>

<h1 align="center">Page2Cam</h1>

<p align="center"><b>Your Webcam. Your Rules. Your AI.</b><br>
<i>Create. Customize. Express. Automate.</i></p>

<p align="center">A creative virtual camera platform that puts control of your visual identity in your hands, or in the hands of your AI agent.</p>

**Page2Cam is a project by Prasad Vittaldev.**
Connect with him on LinkedIn: <https://www.linkedin.com/in/prasadvittaldev/>

---

## Three pillars of Page2Cam

| 1. Creative Freedom | 2. Visual Control | 3. Agentic Control |
|---|---|---|
| Switch between professional, playful, expressive and completely custom webcam experiences. | Transform websites, webcam feeds, backgrounds, overlays and presentations into your own camera experience. | Let AI agents operate and orchestrate the camera experience: changing scenes, adjusting presentation and controlling streaming workflows. |

## What is Page2Cam?

A camera experience you create, control and express, yourself or through AI agents.

Page2Cam is a virtual camera for Teams, Zoom, Google Meet and any other app that uses a webcam, and a live stream to YouTube. Instead of just your webcam, people see what you design: a live website, a dashboard, a slide show or one of your screens, with **you** on top of it (in a box, or cut out of your room), and TV-style graphics over everything.

You run it from a simple control page on your own computer, or you just ask your AI agent. The people watching only see the result, never your controls.

### 1. Creative Freedom

- **Put yourself on the page** in one of three ways:
  - **Box**: your webcam in a small box that you can move and resize anywhere on the page.
  - **Cut-out small**: just you, with your room removed, standing at the bottom of the page.
  - **Cut-out full**: like a Teams background, except the background is the live website.
- **Change the background**: a picture, a colour, a YouTube video, your blurred room, or **one of your screens**, live.
- **48 ready-made graphics** you fill in: name badges and lower thirds, countdowns, agendas, polls, charts, QR codes, product cards, release notes, code cards and more. Their values change live.
- **Make your own**: save your own graphics as templates, and save a whole look (graphics, background, webcam) as a **scene** you can bring back with one click.

### 2. Visual Control

- **Show a website as your camera**, and click, scroll and type in it from the control page while people are watching.
- **Add overlays on top of everything**: titles, a scrolling ticker, animated charts, pictures and video clips.
- **Go live on YouTube** with the same picture, and choose the sound: the website's, your microphone, or both, each with its own volume.
- **Choose the cut-out quality**: **Quality** gives cleaner edges around your hair, **Fast** runs smoothly on any computer. If your computer can't keep up with Quality, Page2Cam switches to Fast by itself so your video stays smooth.

### 3. Agentic Control

- **Let an AI agent run it**: Claude Desktop, Claude Code, Codex, Cursor or VS Code can start Page2Cam, change the page or background, share a screen, add and update graphics, switch scenes and go live, from a prompt.
- **Agents already know how**: Page2Cam teaches them ready-made setups for 24 kinds of users: streamers and podcasters, sales demos and webinars, teachers, telehealth and therapy, live shopping, faith and community events, and software teams (release demos, stand-ups, developer streams). Just say *"I'm starting a telehealth call, set me up"*.

### What you need

- A computer with **Windows 11**. Windows 10 and Mac are not supported.
- A webcam, if you want to appear on the page. It is optional.
- An internet connection.
- A YouTube account, only if you want to stream to YouTube.

---

## How to install (step by step)

### The easy way: the installer (recommended)

1. **Download the installer.**
   Go to the [Releases page](https://github.com/prasadvittaldev/Page2Cam/releases/latest) and download **`Page2Cam-Setup-<version>.exe`**.

2. **Run it.**
   Double-click the downloaded file.
   If Windows shows **"Windows protected your PC"**, click **More info**, then **Run anyway**. This appears because this early version is not signed with a paid certificate.
   Click **Yes** when Windows asks for permission.

3. **Follow the steps.**
   Click **Next** through the pages. The installer sets up everything, including the Page2Cam camera.
   At the end, leave **Start Page2Cam now** ticked and click **Finish**.

4. **Page2Cam waits in the tray** (the icons next to the clock), stopped, so it uses nothing until you need it.
   Click its icon and choose **Start**, then **Open control page**. You can also start it from the **Start menu** or the **desktop shortcut**.

**Updates are automatic.**
When a new version is out, Page2Cam offers it (in the tray and on the control page). Click **Update now** in the tray menu (or **Install update** on the control page): Page2Cam downloads it, checks that it really comes from Page2Cam, installs it and starts again. Windows asks for permission once.
The tray menu also shows the version you are running, and **Check for updates** asks right away instead of waiting for the next automatic check.

### The one-line way (PowerShell, or ask an AI agent)

Open **PowerShell** and paste:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((irm https://raw.githubusercontent.com/prasadvittaldev/Page2Cam/main/scripts/install-windows.ps1)))"
```

It downloads the latest installer from the Releases page, checks its checksum, and installs it. Click **Yes** when Windows asks for permission.
Add **`-AllowAgents`** at the end (inside the last bracket) to also let AI agents control Page2Cam (see below).

**Or just ask your agent.** In Claude Code, Codex, or any agent that can run commands on your PC, type:

> Install Page2Cam for me and tune it for this PC: follow https://raw.githubusercontent.com/prasadvittaldev/Page2Cam/main/AGENT-INSTALL.md

The agent checks your PC (Windows version, processor, memory, graphics, webcam), picks settings that fit it (picture size, frame rate, cut-out quality), installs Page2Cam with agent control on, and tells you to restart your agent app. After the restart it can start Page2Cam, measure how smoothly it runs and fine-tune it. The steps it follows are in [AGENT-INSTALL.md](AGENT-INSTALL.md).

Accept the Windows permission prompt when it appears. After restarting Claude Desktop (or your other agent app), Page2Cam shows up as a tool.

### The portable way: the zip file

Use this if you can't run installers on your computer.

1. From the [Releases page](https://github.com/prasadvittaldev/Page2Cam/releases/latest), download **`Page2Cam-windows-x64.zip`**.
2. Right-click the zip and choose **Properties**. Tick **Unblock** at the bottom, then click **OK**.
3. Right-click the zip, choose **Extract All…**, and extract it to a folder such as `C:\Page2Cam`.
4. In that folder, right-click **`install-camera.cmd`**, choose **Run as administrator**, and click **Yes**. Wait for **"Done. The Page2Cam camera is installed."**
5. Double-click **`page2cam.exe`** to start Page2Cam.

The portable copy doesn't update itself. When a new version is out, the control page shows a link to download it.

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

### Share a screen
In the **Background** panel, choose **Screen** in the full-frame row and pick a monitor.
Everything on that monitor is shown live (Windows draws a yellow border around it while it is shared), with you on top. Choose **Page** again to go back to your website.

### Add overlays (titles, tickers, graphics)
Overlays are drawn on top of everything, you included, like the graphics on a TV broadcast.
1. In the **Overlays** panel, choose **Title** or **Ticker**, type the text (for a ticker, separate the items with `|`), pick a look (**News**, **Minimal** or **Brand**) and click **Add**.
2. Click an overlay in the list to outline it in the preview, then drag it to move it or drag its corner to resize it.
3. Use the tick box to show or hide one overlay, **×** to delete it, and **Hide all** to clear the picture at once.

AI agents can add much more: animated charts and infographics (any HTML), pictures, video clips and YouTube boxes. For example, ask: *"Show a lower third with my name, Ada Lovelace, Chief Analyst, for 10 seconds, add a ticker along the bottom with our three launch headlines, and put a small animated bar chart of this week's sign-ups in the top-right corner."*
Overlays come back hidden when Page2Cam starts again, so nothing appears in a call by surprise. Video sound only goes to YouTube (with the page sound), never into calls.

### Let AI agents control Page2Cam
1. Click the Page2Cam tray icon and tick **Allow agents** (or use the installer's `-AllowAgents`).
2. Restart Claude Desktop, Codex, Cursor or VS Code. Page2Cam registers itself with the ones it finds; Claude Code is registered too when `claude` is installed.
3. Ask, for example: *"Start Page2Cam, open my dashboard at https://example.com, put me in the bottom-right corner, and share my second screen."*

Agents act without asking, so the tray shows a notification when one starts or stops Page2Cam or your stream, or shares your screen, and the **Agents** panel on the control page lists every change. Untick **Allow agents** to switch it off. Details for developers: [`docs/agent-api.md`](docs/agent-api.md).

### What agents can do for you
Agents get a built-in guide with playbooks for 24 kinds of users and 48 ready-made graphics (lower thirds, timers, agendas, charts, polls, QR codes, product cards, code and release-notes cards…) whose values change live. Just say who you are and what you're doing:

- **Creators:** *"Set me up for my game stream: starting-soon card with a 5-minute countdown, then my handles in a corner."* *"I'm teaching a yoga class: interval timer, 40 seconds on, 20 off, 8 rounds."*
- **Business:** *"I'm hosting a webinar on our Q4 plans: agenda on the right, highlight item 2 now."* *"Live shopping: show the headphones card at 79 euros, 7 left, offer ends at 20:00."*
- **Care and education:** *"I'm a physiotherapist starting a telehealth call: blur my room and show my name and practice."* *"Teacher mode: today's steps, and a QR code to the worksheet."*
- **Community:** *"Sunday service: lyrics card for the first song, and a giving QR code at the end."* *"Fundraiser: goal bar at 6,200 of 10,000 dollars; update it as donations come in."*
- **Software companies:** *"We're demoing our 2.0 release to customers: version badge, release notes, then a code card of the new API."* *"Incident update for the all-hands: status card, degraded, with the timeline so far."*

Ask the agent to **save the look** ("save this as Intro") and switch between looks later in one sentence. You can also do all of it yourself in the control page: the **Overlays** panel has **Templates** (fill in a form, click Add; make, import and export your own) and **Scenes** tabs. The full guide the agents read is in [`docs/agent-guide/`](docs/agent-guide/README.md).

### Close Page2Cam
Click the tray icon and choose **Stop** (Page2Cam stays in the tray) or **Quit**.

---

## If something goes wrong

The coloured dots at the bottom of the control page show the state of each part: virtual camera, webcam, stream and microphone. Hover over a dot to read the details.

| What you see | What to do |
|---|---|
| "The Page2Cam camera is not installed" | Run the installer again. With the zip, run `install-camera.cmd` **as administrator**. Then start Page2Cam again. |
| Page2Cam doesn't appear as a camera in Teams or Meet | Start Page2Cam first, then fully close and reopen the call app, or reload the Meet page. |
| The camera shows a dark picture with a moving light band | The camera is installed but Page2Cam isn't running. Click the tray icon and choose **Start**. |
| "cut-out unavailable" | Run the installer again (with the zip: run `install-camera.cmd` as administrator again). |
| "Quality was too slow on this PC, switched to Fast" | This is normal on slower computers. Page2Cam switched to keep your video smooth. You can click **Quality** to try again. |
| "ffmpeg was not found" | Keep `ffmpeg.exe` in the Page2Cam folder. |

If you need help, the file **`page2cam.log`** records what happened. Its location is shown at the bottom of the control page. The camera's own record is in `C:\ProgramData\Page2Cam\vcam.log`.

---

## Your privacy

- Page2Cam runs entirely on your own computer.
- The control page only works from your own computer. Other computers and other websites cannot control it.
- Your YouTube stream key is never written to the log. If you tick *Remember key*, it is kept in Windows' own secure password store.
- Every few hours Page2Cam asks GitHub for the latest version number, and nothing else. It doesn't send your settings, your pictures or anything you do. To turn this off, add `check_updates = false` to the settings file.
- The cut-out (background removal) happens on your computer. Your video is not sent anywhere to be processed.

---

## How to remove Page2Cam

**If you used the installer:** open **Settings → Apps → Installed apps**, find **Page2Cam**, click **⋯** and choose **Uninstall**. This also removes the Page2Cam camera. Your settings are kept in case you install it again.

**If you used the zip:** close Page2Cam, right-click **`uninstall-camera.cmd`**, choose **Run as administrator**, then delete the folder.

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
