# Windows tray app with Start/Stop, standby camera and health icon — design

Date: 2026-11-19. Status: awaiting user approval (builder check done).

## Why
Page2Cam is used for calls and streams, not all day. Today starting `page2cam.exe` immediately captures the webcam, renders the page (Chromium), runs the cut-out and the virtual camera, and keeps a console window open. The user wants it to be loadable at sign-in but **idle until explicitly started**, controlled from the **system tray** (Start, Stop, Force restart), with the **"Page2Cam" camera always listed** in Teams/Zoom (a standby picture while stopped), and a **health icon** that turns amber when the PC is under strain. Today's acceptance session also showed failures (Windows camera service stopping, webcam dying) that Page2Cam never recovered from; a supervising tray process fixes that class of problem.

## Goals
- `page2cam.exe` starts as a **tray process** (no console window) in the **Stopped** state. Nothing heavy runs: no Chromium, no webcam, no GPU work.
- Tray menu: status line, **Start**, **Stop**, **Open control page** (when running), **Force restart**, **Start with Windows** (checkbox), **Quit**.
- The "Page2Cam" virtual camera stays **registered while the tray runs**; while stopped it shows a **standby picture** (logo + "Page2Cam is stopped — start it from the tray icon"), so Teams keeps it selected.
- **Start** launches the **engine** (today's app) as a child process; it writes live frames into the same camera. **Stop** shuts the engine down cleanly; the standby picture returns.
- **Force restart**: kill the engine and every related process (Chromium helpers, orphaned/leftover Page2Cam processes), re-register the camera, start the engine again. The tray itself survives.
- **Health icon**: grey (stopped), green (running fine), **amber** (running under strain), red (error). Tooltip lists the reasons.
- **Load at sign-in** without starting: users are told explicitly (installer text + a first-run notification "Page2Cam is in your tray. Click Start when you need it.").
- Engine crash while started → tray restarts it once automatically; a second crash within 2 min → red, stays stopped, notification.

## Non-goals
- Linux/macOS tray (Linux keeps today's console app = engine only).
- Restarting Windows' Frame Server service (needs admin).
- Changing the engine's admin page features (crop, GPU picker etc. stay as they are).

## Anti-goals
- A "stopped" Page2Cam that still burns CPU/GPU/RAM noticeably (target: tray < 1 % CPU, < 40 MB RAM while stopped).
- The camera disappearing from Teams between meetings while the tray runs.
- Two trays, or a tray and a standalone engine, fighting over the camera or the admin port.
- Health amber flapping every few seconds (needs hysteresis).

## Approaches (user picked A)
- **A (chosen): tray process owns the camera registration; the engine is a separate child process.** Clean Stop (Chromium can only initialise once per process, so a real stop needs the process to end), supervision and recovery for free.
- B: one process where Stop pauses the engine — rejected: Chromium must keep running (~300 MB, GPU) and can't switch GPU/adapters between sessions.
- C: Windows service — rejected: needs admin and runs outside the user session, but the virtual camera is per user session.

## Design

### Decisions settled after review (designer's calls — check while reading)
1. **One frame writer at a time (handover).** The shared-memory header gains a `writer: u32` field (0 none, 1 tray, 2 engine), set only by the tray. While stopped/starting the tray writes standby frames (writer = 1). The engine's camera sink writes only while `writer == 2`. The tray switches to 2 when the engine's `/api/state` reports `output_ready: true` (first output frame produced) and stops writing; on Stop/exit/crash it sets 1 and resumes standby. No two writers ever touch the seqlock at once. The "camera driver out of date" check (`dll_version`) moves to the tray (red health); the engine keeps logging it.
2. **Order of work:** the tray plan runs **after** the crop plan (whose Tasks 3–5 add the camera start-retry, leftover-process stop and Frame-Server recovery that the tray reuses).
3. **Admin token/port injection:** `AdminConfig` gets `token: Option<String>` and `strict_port: bool`. The tray picks a free port right before spawning (bind-and-release test) and passes `--admin-port`; if the engine still fails to bind it exits with code 76 and the tray retries once with a new port. The token travels in the env var only (visible to the same user's processes only — same threat level as today's per-launch token). `/api/quit` is guarded like every route.
4. **Windows subsystem and `--standalone`:** the exe uses the GUI subsystem (CEF helpers and relaunches never show a console). `--standalone` attaches to the parent console if there is one (`AttachConsole(ATTACH_PARENT_PROCESS)`) so CLI use still prints the admin URL, and refuses to start (message box) while a tray holds `Local\Page2Cam.Tray`. In tray mode the stdout admin-URL print is dropped.
5. **Engine exit codes:** 0 after a tray-requested Stop = normal; 0 not requested = treated as stopped (grey, no restart); 75 = "restart requested" (GPU change) → tray restarts it, not counted as a crash; 76 = port bind failure (see 3); anything else = crash (auto-restart once, second crash within 2 min → red). Exit while Stopping is always normal.
6. **Updates move to the tray:** the tray runs the update checks (15 s after start, every 6 h, `check_updates` setting) and the install flow (download + verify + elevated installer `/S /RELAUNCH`, as today), so updates work while stopped; the engine's updater is disabled in tray mode (`--parent-pid` given). The installer already stops all `page2cam.exe` processes (tray included — removing the session camera), re-registers the DLL, and `/RELAUNCH` starts the tray (stopped). UAC prompt on install as today.
7. **Tray state** lives in its own file `<config dir>/page2cam/tray.toml` (`tray_seen`); "Start with Windows" uses the Run key as its only source of truth. The engine's settings file is never written by the tray.
8. **Standby pictures:** two, pre-rendered by `scripts/make-brand-assets.py` as 1920×1080 PNGs and converted to NV12 (BT.709) once at tray start: "Page2Cam is stopped — start it from the tray icon" and "Page2Cam needs attention — open the tray icon" (red health). Scaling for 720p consumers is done by the DLL as for live frames.

### Processes and roles
- `page2cam.exe` (no role flag, Windows): **tray**. Single instance via a named mutex `Local\Page2Cam.Tray`; a second launch signals the running tray to open its control page (or its menu if stopped) and exits.
- `page2cam.exe --engine --camera-from-tray --admin-port <p> --parent-pid <tray pid>` with env `P2C_ADMIN_TOKEN=<token>`: **engine** = today's app, except it does not register the virtual camera (the tray owns `MFCreateVirtualCamera`) and only writes frames to the shared memory; it exits if the parent pid disappears (orphan protection).
- `page2cam.exe --standalone` (and all non-Windows builds): today's behaviour (engine + own camera registration + console), for debugging and Linux.
- CEF helper processes (`--type=…`) unchanged.
- The exe is built with the Windows GUI subsystem (no console); logs go to `page2cam.log` as today; the tray writes `page2cam-tray.log`. `--standalone` allocates a console (`AllocConsole`) so CLI use still shows the admin URL.

### Tray process
- Icon + menu via the `tray-icon`/`muda` crates on a dedicated thread with a Win32 message loop.
- Owns the virtual camera: the existing `Owner` (MFCreateVirtualCamera on an MTA thread) plus the start-retry and Frame-Server-restart recovery from the crop plan's Tasks 3/5, reused.
- Standby frames: a pre-rendered NV12 1920×1080 picture (logo + text, BT.709 limited, built once at start from the embedded logo PNG with simple text rendering, or a pre-rendered PNG asset made by `scripts/make-brand-assets.py`), written to the frame mapping once per second while the engine is not running. The camera DLL's existing "waiting" pattern remains the fallback if even the tray is gone.
- Admin token and port are generated by the tray and passed to the engine (env var for the token — never on the command line). "Open control page" opens `http://127.0.0.1:<port>/?t=<token>`.
- Start: spawn the engine (CREATE_NO_WINDOW), state Starting → Running once `/api/state` answers (timeout 30 s → red). Stop: `POST /api/quit` (new engine route; normal shutdown), wait up to 10 s, then kill the engine's process tree. Quit: Stop, then remove the camera (`IMFVirtualCamera::Remove`), exit.
- Force restart: kill the engine tree immediately; stop every other `page2cam.exe` process that is not the tray itself or its ancestors (reuse `instances::to_stop`, extended to exclude ancestors); drop and re-create the camera registration (Task 3 retry loop); start the engine.
- Start with Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `Page2Cam` = `"<exe path>"` (tray, stopped). Toggle in the menu; the installer sets it per a checkbox (default on) and explains it.
- First run (no `tray_seen = true` in settings): balloon notification "Page2Cam is in your tray. Click Start when you need it.", then sets `tray_seen`.
- Update banner: when the engine's status reports an update, the tray menu shows "Install update <version>"; the existing updater flow runs in the engine (unchanged), and the installer relaunches the tray (`/RELAUNCH` → tray, stopped).

### Health
- Engine adds `health` to `/api/state`: `{ output_skipped_pct, webcam_fps, webcam_state, cutout_auto_fallback, video_fallback, camera_status }` from data it already logs every 30 s (use a 10 s window for health).
- Tray computes, every 2 s:
  - **red**: engine not answering for 5 s while started; engine exited unexpectedly; camera driver not installed/out of date; engine failed to start.
  - **amber** (any): output skipped > 2 % of frames; webcam fps < 12 while the webcam is on; webcam reconnecting; cut-out on automatic Fast fallback; video on CPU fallback; camera retrying; system CPU utility > 85 % or CPU clock < 70 % of base (tray reads PDH counters `\Processor Information(_Total)\% Processor Utility` / `% Processor Performance`).
  - **green** otherwise; **grey** while stopped.
- Hysteresis: amber/red must hold for 6 s to show and clear after 10 s of good readings. Tooltip (≤ 127 chars) lists the top reasons, e.g. "Page2Cam — running · strain: CPU throttled, webcam 9 fps".
- Icon variants: the app icon with a small coloured dot (grey/green/amber/red), generated at runtime from the embedded icon.

### Interaction with existing features
- "Stop leftover Page2Cam processes at start" (crop plan Task 4): the engine run by the tray must not stop the tray or itself (exclude ancestors); the tray runs the cleanup at its own start (before registering the camera).
- R28/WER self-relaunch: disabled when `--parent-pid` is given (the tray supervises); kept for `--standalone`.
- Admin page "Restart now" (GPU change): in tray mode the engine asks the tray to restart it (POST to the tray? — simplest: the engine exits with a "restart requested" exit code 75 and the tray starts it again).
- Engine status shown in the tray reuses `video`/`encoder`/`cutout`/`vcam` status strings.

### Testing
- Pure logic unit-tested: health classification + hysteresis (table tests), tray state machine (Stopped/Starting/Running/Stopping/Error with injected engine events), argument/env building (token never on the command line), ancestor-aware `to_stop`, Run-key value.
- Windows integration (ignored tests / manual): start/stop/force-restart with a debug engine; camera stays listed in the Windows Camera app across Stop/Start with the standby picture; Quit removes it.
- Acceptance with the user: sign-in autostart stopped; Start/Stop in a Teams call without re-selecting the camera; amber under a background compile; Force restart while the camera service is broken.

## Left to the builder
- Text rendering for the standby picture (pre-rendered asset preferred if simpler), tray icon dot drawing, exact tray-crate versions (must build in the cargo-xwin cross-build), menu wording within the items above, PDH vs `GetSystemTimes` for CPU utility.
