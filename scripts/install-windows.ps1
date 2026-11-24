<#
.SYNOPSIS
  Download and install Page2Cam on Windows 11 (for people and AI agents).

.DESCRIPTION
  Fetches the installer from the project's GitHub release, checks it against
  the release's SHA256SUMS, and runs it silently. Windows asks once for
  administrator permission (the installer sets up the Page2Cam camera).
  Afterwards Page2Cam waits in the tray, stopped, until it is started.

  One line, from PowerShell (or from an agent's shell):
    powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((irm https://raw.githubusercontent.com/prasadvittaldev/Page2Cam/main/scripts/install-windows.ps1))) -AllowAgents"

.PARAMETER Version
  A release tag such as v0.5.0, or "latest" (default).

.PARAMETER AllowAgents
  Also turn on "Allow agents", so desktop agents (Claude Desktop, Claude Code,
  Codex, Cursor, VS Code) can control Page2Cam over MCP. Page2Cam registers
  itself with them when its tray starts; restart those apps afterwards.
  Same as ticking "Allow agents" in the tray menu; can be switched off there.

.PARAMETER NoLaunch
  Don't start Page2Cam's tray after installing.

.NOTES
  If the repository is private, the GitHub CLI (gh, logged in) or a token in
  GH_TOKEN / GITHUB_TOKEN is used for the download.
#>
[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [switch]$AllowAgents,
    [switch]$NoLaunch
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without it
$Repo = 'prasadvittaldev/Page2Cam'

function Say($m) { Write-Host "[page2cam] $m" }

# --- Checks -----------------------------------------------------------------
if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'Page2Cam installs on Windows 11 only.' }
if ([Environment]::OSVersion.Version.Build -lt 22000) { throw 'Page2Cam needs Windows 11 (build 22000 or newer).' }
if (-not [Environment]::Is64BitOperatingSystem) { throw 'Page2Cam needs 64-bit Windows.' }
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$work = Join-Path ([IO.Path]::GetTempPath()) ("page2cam-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null

# --- Download ---------------------------------------------------------------
$token = if ($env:GH_TOKEN) { $env:GH_TOKEN } elseif ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } else { $null }
$gh = Get-Command gh -ErrorAction SilentlyContinue
$useGh = $false
if (-not $token -and $gh) {
    & gh auth status *> $null
    $useGh = ($LASTEXITCODE -eq 0)
}

$headers = @{ 'User-Agent' = 'page2cam-install'; 'Accept' = 'application/vnd.github+json' }
if ($token) { $headers['Authorization'] = "Bearer $token" }
$api = if ($Version -eq 'latest') { "https://api.github.com/repos/$Repo/releases/latest" }
       else { "https://api.github.com/repos/$Repo/releases/tags/$Version" }

if ($useGh) {
    Say "Downloading with the GitHub CLI ($Version)..."
    $tagArg = if ($Version -eq 'latest') { @() } else { @($Version) }
    & gh release download @tagArg --repo $Repo --dir $work --pattern 'Page2Cam-Setup-*.exe' --pattern 'SHA256SUMS'
    if ($LASTEXITCODE -ne 0) { throw "gh release download failed (exit $LASTEXITCODE)." }
    $setup = Get-ChildItem $work -Filter 'Page2Cam-Setup-*.exe' | Select-Object -First 1
    $sums = Join-Path $work 'SHA256SUMS'
} else {
    try { $rel = Invoke-RestMethod -Uri $api -Headers $headers }
    catch { throw "Could not read the $Version release of $Repo from GitHub ($($_.Exception.Message)). If the repository is private, log in with 'gh auth login' or set GH_TOKEN." }
    Say "Release $($rel.tag_name)"
    $setupAsset = $rel.assets | Where-Object { $_.name -like 'Page2Cam-Setup-*.exe' } | Select-Object -First 1
    $sumsAsset = $rel.assets | Where-Object { $_.name -eq 'SHA256SUMS' } | Select-Object -First 1
    if (-not $setupAsset -or -not $sumsAsset) { throw "Release $($rel.tag_name) has no Page2Cam-Setup-*.exe or SHA256SUMS." }
    $dl = @{ 'User-Agent' = 'page2cam-install'; 'Accept' = 'application/octet-stream' }
    if ($token) { $dl['Authorization'] = "Bearer $token" }
    foreach ($a in @($setupAsset, $sumsAsset)) {
        Say "Downloading $($a.name) ($([math]::Round($a.size / 1MB, 1)) MB)..."
        # The API asset URL works for public and (with a token) private repositories.
        Invoke-WebRequest -Uri $a.url -Headers $dl -OutFile (Join-Path $work $a.name) -UseBasicParsing
    }
    $setup = Get-Item (Join-Path $work $setupAsset.name)
    $sums = Join-Path $work 'SHA256SUMS'
}
if (-not $setup) { throw 'The installer was not downloaded.' }

# --- Verify -----------------------------------------------------------------
$line = Get-Content $sums | Where-Object { $_ -match ('\s\*?' + [regex]::Escape($setup.Name) + '$') } | Select-Object -First 1
if (-not $line) { throw "SHA256SUMS has no entry for $($setup.Name)." }
$want = ($line -split '\s+')[0].ToLowerInvariant()
$got = (Get-FileHash -Algorithm SHA256 $setup.FullName).Hash.ToLowerInvariant()
if ($want -ne $got) { throw "Checksum mismatch for $($setup.Name) (want $want, got $got). Not installing." }
Say "Checksum OK ($($setup.Name))"

# --- Allow agents (before the installer starts the tray) --------------------
if ($AllowAgents) {
    $dir = Join-Path $env:APPDATA 'page2cam\config'
    $file = Join-Path $dir 'tray.toml'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $text = if (Test-Path $file) { [IO.File]::ReadAllText($file) } else { '' }
    if ($text -match '(?m)^\s*agents_allowed\s*=') {
        $text = [regex]::Replace($text, '(?m)^\s*agents_allowed\s*=.*$', 'agents_allowed = true')
    } else {
        # Top of the file: before any [table], so it stays a top-level key.
        $text = "agents_allowed = true`r`n" + $text
    }
    [IO.File]::WriteAllText($file, $text, (New-Object Text.UTF8Encoding($false)))
    Say "'Allow agents' turned on ($file)"
}

# --- Install ----------------------------------------------------------------
$setupArgs = @("/S")
if (-not $NoLaunch) { $setupArgs += "/RELAUNCH" }
Say 'Installing (Windows asks for administrator permission)...'
try {
    $p = Start-Process -FilePath $setup.FullName -ArgumentList $setupArgs -Verb RunAs -Wait -PassThru
} catch {
    throw "The installer did not start: $($_.Exception.Message) (was the permission prompt declined?)"
}
if ($p.ExitCode -ne 0) { throw "The installer failed (exit code $($p.ExitCode))." }

Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
$exe = Join-Path $env:ProgramFiles 'Page2Cam\page2cam.exe'
if (-not (Test-Path $exe)) { throw "The installer finished but $exe is missing. Run this again." }

# --- Camera -----------------------------------------------------------------
# The installer registers the camera component (regsvr32); the camera named
# Page2Cam then appears while Page2Cam's tray runs.
$clsidKey = 'HKLM:\SOFTWARE\Classes\CLSID\{7C2F9A41-3B6E-4D8A-9C1F-5E2B8D4A6F10}\InProcServer32'
if (-not (Test-Path $clsidKey)) {
    throw 'The Page2Cam camera was not registered. Restart the PC and run this again.'
}
$camera = $null
if (-not $NoLaunch) {
    for ($i = 0; $i -lt 20 -and -not $camera; $i++) {
        Start-Sleep -Seconds 1
        $camera = Get-PnpDevice -Class Camera -PresentOnly -ErrorAction SilentlyContinue |
            Where-Object FriendlyName -eq 'Page2Cam' | Select-Object -First 1
    }
}
if ($camera) {
    Say 'Camera: Page2Cam is ready (choose it in Teams, Zoom or Meet).'
} elseif ($NoLaunch) {
    Say 'Camera: registered; the camera Page2Cam appears once Page2Cam is started.'
} else {
    Say 'Camera: registered, but not shown yet. It appears while Page2Cam runs in the tray; if it is missing, open Page2Cam from the Start menu.'
}
Say "Installed: $exe"
if ($NoLaunch) {
    Say 'Start it from the Start menu (Page2Cam) when you want it.'
} else {
    Say 'Page2Cam is waiting in the tray (stopped). Click its tray icon and choose Start.'
}
if ($AllowAgents) {
    Say 'Agents: restart Claude Desktop / Codex / Cursor / VS Code so they pick up the page2cam MCP server.'
    Say "Any other MCP client: command `"$exe`" with the argument --mcp."
}
