# Renders the tray's standby pictures (1920x1080) for the virtual camera.
# Windows PowerShell 5.1 + System.Drawing; no extra installs.
# Output is committed: crates/p2c-tray/assets/standby-{stopped,attention}.png
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent $PSScriptRoot
$logo = [System.Drawing.Image]::FromFile((Join-Path $root 'assets\logo.png'))
$outDir = Join-Path $root 'crates\p2c-tray\assets'
New-Item -ItemType Directory -Force $outDir | Out-Null

function New-Standby([string]$text, [string]$file) {
    $bmp = New-Object System.Drawing.Bitmap 1920, 1080, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.ColorTranslator]::FromHtml('#0d1b2a'))
    $g.SmoothingMode = 'HighQuality'
    $g.InterpolationMode = 'HighQualityBicubic'
    $lw = 1100
    $lh = [int]($logo.Height * $lw / $logo.Width)
    $pad = 50
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $cx = [int]((1920 - $lw) / 2) - $pad; $cy = 220 - $pad; $cw = $lw + 2 * $pad; $ch = $lh + 2 * $pad; $r = 60
    $path.AddArc($cx, $cy, $r, $r, 180, 90); $path.AddArc($cx + $cw - $r, $cy, $r, $r, 270, 90)
    $path.AddArc($cx + $cw - $r, $cy + $ch - $r, $r, $r, 0, 90); $path.AddArc($cx, $cy + $ch - $r, $r, $r, 90, 90); $path.CloseFigure()
    $g.FillPath([System.Drawing.Brushes]::White, $path)
    $g.DrawImage($logo, [int]((1920 - $lw) / 2), 220, $lw, $lh)
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $font = New-Object System.Drawing.Font('Segoe UI', 64, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = 'Center'
    $fmt.LineAlignment = 'Center'
    $rect = New-Object System.Drawing.RectangleF 0, 600, 1920, 120
    $g.DrawString($text, $font, [System.Drawing.Brushes]::White, $rect, $fmt)
    $g.Dispose()
    $bmp.Save((Join-Path $outDir $file), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

$dash = [string][char]0x2014
New-Standby ("Page2Cam is stopped $dash start it from the tray icon") 'standby-stopped.png'
New-Standby ("Page2Cam needs attention $dash open the tray icon") 'standby-attention.png'
$logo.Dispose()
