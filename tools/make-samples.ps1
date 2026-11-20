# Generate synthetic test photos into .\samples (JPEG + PNG, assorted sizes, one broken file).
# Usage:  powershell -ExecutionPolicy Bypass -File .\tools\make-samples.ps1
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$out = Join-Path (Split-Path $PSScriptRoot) "samples"
New-Item -ItemType Directory -Force $out | Out-Null

function New-Canvas($w, $h) {
    $b = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.SmoothingMode = "AntiAlias"
    return $b, $g
}
function Fill-Gradient($g, $w, $h, $top, $bottom) {
    $r = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
    $br = New-Object System.Drawing.Drawing2D.LinearGradientBrush($r, $top, $bottom, 90)
    $g.FillRectangle($br, $r); $br.Dispose()
}
function Brush($a, $r, $g, $b) { New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb($a, $r, $g, $b)) }
function Pts($list) { [System.Drawing.PointF[]]@($list | ForEach-Object { New-Object System.Drawing.PointF($_[0], $_[1]) }) }
function Save-Jpeg($bmp, $path, $quality = 88) {
    $codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq "image/jpeg"
    $p = New-Object System.Drawing.Imaging.EncoderParameters(1)
    $p.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter([System.Drawing.Imaging.Encoder]::Quality, [long]$quality)
    $bmp.Save($path, $codec, $p)
}

# 1. Sunset over mountains, large landscape (tests downscaling)
$b, $g = New-Canvas 2400 1600
Fill-Gradient $g 2400 1600 ([System.Drawing.Color]::FromArgb(255, 40, 30, 90)) ([System.Drawing.Color]::FromArgb(255, 250, 150, 70))
$g.FillEllipse((Brush 255 255 220 120), 1500, 700, 360, 360)
$g.FillPolygon((Brush 255 70 40 70), (Pts @(@(0,1600), @(0,1000), @(500,650), @(900,1050), @(1300,750), @(1800,1150), @(2400,800), @(2400,1600))))
$g.FillPolygon((Brush 255 35 20 40), (Pts @(@(0,1600), @(0,1250), @(700,1000), @(1200,1300), @(1700,1080), @(2400,1350), @(2400,1600))))
Save-Jpeg $b "$out\sunset.jpg"; $g.Dispose(); $b.Dispose()

# 2. Forest lake, 4:3
$b, $g = New-Canvas 1600 1200
Fill-Gradient $g 1600 700 ([System.Drawing.Color]::FromArgb(255, 120, 190, 240)) ([System.Drawing.Color]::FromArgb(255, 220, 240, 250))
$g.FillRectangle((Brush 255 40 110 150), 0, 700, 1600, 500)
$rnd = New-Object System.Random(7)
for ($i = 0; $i -lt 40; $i++) {
    $x = $rnd.Next(-50, 1600); $hgt = $rnd.Next(250, 520); $wid = $hgt * 0.4
    $shade = $rnd.Next(60, 120)
    $g.FillPolygon((Brush 255 20 $shade 40), (Pts @(@($x, (700 - $hgt)), @(($x - $wid / 2), 710), @(($x + $wid / 2), 710))))
    $g.FillPolygon((Brush 90 20 $shade 40), (Pts @(@($x, (700 + $hgt * 0.6)), @(($x - $wid / 2), 700), @(($x + $wid / 2), 700))))
}
Save-Jpeg $b "$out\forest-lake.jpg"; $g.Dispose(); $b.Dispose()

# 3. Portrait orientation
$b, $g = New-Canvas 900 1350
Fill-Gradient $g 900 1350 ([System.Drawing.Color]::FromArgb(255, 250, 230, 200)) ([System.Drawing.Color]::FromArgb(255, 200, 120, 90))
$g.FillEllipse((Brush 255 120 70 50), 250, 250, 400, 480)
$g.FillEllipse((Brush 255 60 140 200), 150, 800, 600, 700)
$g.FillEllipse((Brush 255 255 255 255), 340, 420, 60, 40); $g.FillEllipse((Brush 255 255 255 255), 500, 420, 60, 40)
Save-Jpeg $b "$out\portrait.jpg"; $g.Dispose(); $b.Dispose()

# 4. PNG with transparency
$b, $g = New-Canvas 512 512
$g.Clear([System.Drawing.Color]::Transparent)
$g.FillEllipse((Brush 255 46 134 222), 16, 16, 480, 480)
$g.FillEllipse((Brush 160 255 255 255), 120, 120, 272, 272)
$font = New-Object System.Drawing.Font("Arial", 72, [System.Drawing.FontStyle]::Bold)
$g.DrawString("PNG", $font, (Brush 255 20 40 80), 120, 200)
$b.Save("$out\badge.png", [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $b.Dispose()

# 5. Tiny PNG (should not be blown up into a blur)
$b, $g = New-Canvas 64 48
$g.Clear([System.Drawing.Color]::FromArgb(255, 255, 200, 0))
$g.FillRectangle((Brush 255 200 0 80), 8, 8, 24, 32)
$b.Save("$out\tiny.png", [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $b.Dispose()

# 6. Broken file (truncated JPEG) - must show an error tile, not crash
$bytes = [IO.File]::ReadAllBytes("$out\portrait.jpg")
[IO.File]::WriteAllBytes("$out\broken.jpg", $bytes[0..2000])

# 7. Not an image at all, just named like one - must show "Can't open"
[IO.File]::WriteAllText("$out\unreadable.jpg", "This is a text file pretending to be a photo.")

Get-ChildItem $out | Format-Table Name, Length
