# Page2Cam: install and tune guide for AI agents

You are an AI agent (Claude Code, Codex, Cursor, VS Code agent mode, or any agent that can run PowerShell on the user's PC). This file tells you how to install Page2Cam on this Windows PC and fit its settings to how powerful the PC is. Follow it in order. Tell the user what you are doing at each step, in plain words.

Users can start you with: *"Install Page2Cam for me: follow https://raw.githubusercontent.com/prasadvittaldev/Page2Cam/main/AGENT-INSTALL.md"*

## Ground rules

- Run commands in **PowerShell** on the user's PC. Windows will ask the user **once** for administrator permission: tell them to click **Yes**.
- **The Page2Cam camera is part of Page2Cam, not an extra driver.** The installer in Step 4 sets it up by itself (it registers Page2Cam's camera component with Windows). That is required and expected, so do run it. Don't install or download any other camera software or driver.
- Apart from that, don't change anything else on the PC: no other drivers, no power plans, no other apps' settings. Suggest those to the user instead.
- Don't install or tune while the user is in a call or live stream: some steps restart Page2Cam. Ask first if unsure.
- Don't stop or kill a running Page2Cam without asking.

## Step 1. Check the PC (read only)

Run this and keep the output:

```powershell
$os  = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$gpu = Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) [$([math]::Round($_.AdapterRAM/1GB,1)) GB reported]" }
$bat = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue
$cam = Get-PnpDevice -Class Camera,Image -PresentOnly -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FriendlyName
[pscustomobject]@{
  Windows   = "$($os.Caption) build $($os.BuildNumber) ($($os.OSArchitecture))"
  CPU       = "$($cpu.Name.Trim()) - $($cpu.NumberOfCores) cores / $($cpu.NumberOfLogicalProcessors) threads"
  RAM_GB    = [math]::Round($os.TotalVisibleMemorySize/1MB)
  GPUs      = $gpu -join '; '
  Laptop    = [bool]$bat
  OnBattery = [bool]($bat | Where-Object { $_.BatteryStatus -eq 1 })
  Webcams   = $cam -join '; '
  Installed = Test-Path "$env:ProgramFiles\Page2Cam\page2cam.exe"
} | Format-List
```

(`AdapterRAM` is capped at 4 GB by Windows; use the GPU name, not that number.)

**Stop here** and explain why if:
- Windows is not Windows 11 (build 22000 or newer) or not 64-bit. Page2Cam runs only on Windows 11 x64.
- There is no webcam: Page2Cam still works (page, screen or picture as the camera), but say so.

## Step 2. Pick a profile

Use the **first** row that matches. A *discrete GPU* is an NVIDIA GeForce/RTX/Quadro or AMD Radeon RX/Pro card. *Onboard* means Intel UHD/Iris/Arc integrated, or AMD "Radeon Graphics" built into the CPU.

| Profile | When | Output | Cut-out | Notes |
|---|---|---|---|---|
| **Light** | ≤ 4 cores, or ≤ 8 GB RAM | 1280×720 @ 24 fps | Fast | Suggest the webcam **box** instead of a cut-out; avoid animated 3D pages |
| **Standard** | onboard graphics only | 1920×1080 @ 30 fps | Fast | Refined edges off; 1280×720 if it struggles |
| **Strong** | a discrete GPU, ≥ 6 cores, ≥ 16 GB | 1920×1080 @ 30 fps | Quality | Refined edges may be turned on later if measured fine |

Laptops on battery: tell the user to plug in and set Windows **power mode to Best performance** (Settings, System, Power & battery). On battery Windows slows the GPU, and Page2Cam can lose half its speed. Laptops with two GPUs (onboard + NVIDIA/AMD): Page2Cam picks the discrete one by itself (`gpu = "auto"`).

## Step 3. Write the starting settings

Do this **before** installing (or, if Page2Cam is already installed, while it is **stopped**: Page2Cam rewrites this file while running). It sets only these keys and keeps everything else in the file. Change the values to your profile:

```powershell
$settings = [ordered]@{
  width = 1920; height = 1080; fps = 30          # Light: 1280, 720, 24
  cutout_engine = '"fast"'                       # Strong: '"quality"'
  refined_edges = 'false'
}
$dir  = Join-Path $env:APPDATA 'page2cam\config'
$file = Join-Path $dir 'config.toml'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$text = if (Test-Path $file) { [IO.File]::ReadAllText($file) } else { '' }
foreach ($k in $settings.Keys) {
  $line = "$k = $($settings[$k])"
  if ($text -match "(?m)^\s*$k\s*=") { $text = [regex]::Replace($text, "(?m)^\s*$k\s*=.*$", $line) }
  else { $text = $line + "`r`n" + $text }
}
[IO.File]::WriteAllText($file, $text, (New-Object Text.UTF8Encoding($false)))
Get-Content $file
```

Only these keys are valid for this step: `width`, `height` (even numbers), `fps` (1–60), `cutout_engine` (`"quality"` or `"fast"`), `refined_edges` (`true`/`false`), `gpu` (`"auto"`), `video_kbps` (stream bitrate, default 6000; 3500 for 720p or a slow upload). An unknown key or a bad value stops Page2Cam from starting.

## Step 4. Install

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((irm https://raw.githubusercontent.com/prasadvittaldev/Page2Cam/main/scripts/install-windows.ps1))) -AllowAgents"
```

It downloads the latest release from GitHub, checks its SHA-256 checksum, installs it silently (the one permission prompt) **including the Page2Cam camera**, checks that the camera is registered, turns on **Allow agents** and leaves Page2Cam waiting in the tray, **stopped**. It ends with `Installed: C:\Program Files\Page2Cam\page2cam.exe`, after a `Camera: ...` line.

Then check the camera (read only):

```powershell
$clsid = '{7C2F9A41-3B6E-4D8A-9C1F-5E2B8D4A6F10}'
[pscustomobject]@{
  Installed   = Test-Path "$env:ProgramFiles\Page2Cam\page2cam.exe"
  Registered  = Test-Path "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InProcServer32"
  TrayRunning = [bool](Get-Process page2cam -ErrorAction SilentlyContinue)
  CameraShown = [bool](Get-PnpDevice -Class Camera -PresentOnly -ErrorAction SilentlyContinue | Where-Object FriendlyName -eq 'Page2Cam')
} | Format-List
```

- `Installed` and `Registered` must be `True`. If not, the install did not finish: run the Step 4 command again (and make sure the user clicked **Yes**). If `Registered` stays `False`, ask the user to restart the PC and run it once more.
- The camera named **Page2Cam** exists only while Page2Cam's tray is running, so `CameraShown` turns `True` a few seconds after the tray starts. If `TrayRunning` is `False`, ask the user to open Page2Cam from the Start menu, then check again.

If it fails, report the message as it is. Common causes: the permission prompt was declined (run it again), no internet, or not Windows 11.

## Step 5. Connect, then tell the user

Page2Cam registers itself as an MCP server (`page2cam`) with Claude Desktop, Claude Code, Codex, Cursor and VS Code when its tray starts. Your app sees the new tools only after it restarts:

- Tell the user to **fully quit and reopen** their agent app (Claude Desktop: quit from its tray icon; Claude Code: `/mcp`, then reconnect `page2cam`, or restart).
- Other MCP clients: command `"C:\Program Files\Page2Cam\page2cam.exe"`, argument `--mcp`.

Then tell the user, in short: Page2Cam is installed, it waits in the tray (bottom-right, maybe under the ^ arrow) until **Start**, in Teams/Zoom/Meet they choose the camera **Page2Cam**, and which profile you chose and why.

## Step 6. Measure and fine-tune (after the restart, with the Page2Cam tools)

1. `status`, then `start` (up to 30 s). Set up what the user usually shows (or follow `guide` for their use case).
2. Wait 30 s, call `status` and read `status.health.output_skipped_pct` (≤ 2 is good), `status.health.webcam_fps`, `status.cutout`, `status.video`.
3. If it struggles, call **`guide` with topic `performance`**: it lists the cheapest fixes first (lighter page, refined edges off, Fast cut-out, box instead of cut-out, the right GPU, then 720p / lower fps) and which ones need a restart.
4. On a **Strong** PC that measured well (≤ 2 % skipped with the cut-out on), you may offer the user `set_cutout_quality quality` and `set_background {"refined_edges": true}` for nicer edges, and measure again.
5. Tell the user what you measured and changed, and what it costs (e.g. "720p instead of 1080p", "Fast cut-out: slightly rougher hair edges").

## Removing it

Settings, Apps, Installed apps, **Page2Cam**, Uninstall. Restart the agent apps afterwards; their `page2cam` entry then points at a missing program and can be removed there.
