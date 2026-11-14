# Draws the logview icon and writes assets/logview.ico (16-256 px, PNG
# entries) and assets/logview.png (256 px, window icon).
#
#   powershell -ExecutionPolicy Bypass -File scripts/make-icon.ps1

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root "assets"
New-Item -ItemType Directory -Force $assets | Out-Null

function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0

    # Rounded dark background.
    $r = 48 * $s
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $w = $size - 1
    $path.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($w - 2 * $r, 0, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($w - 2 * $r, $w - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc(0, $w - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.Point 0, 0), (New-Object System.Drawing.Point 0, $size), ([System.Drawing.Color]::FromArgb(255, 0x26, 0x32, 0x45)), ([System.Drawing.Color]::FromArgb(255, 0x12, 0x18, 0x24))
    $g.FillPath($bg, $path)

    # Log lines: level marker + text bar.
    $lines = @(
        @(0xF4, 0x43, 0x36, 150),
        @(0x4C, 0xAF, 0x50, 120),
        @(0xFF, 0x98, 0x00, 165),
        @(0x4C, 0xAF, 0x50, 95),
        @(0x42, 0xA5, 0xF5, 135)
    )
    $y = 52 * $s
    foreach ($l in $lines) {
        $h = [Math]::Max(1.0, 18 * $s)
        $marker = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, $l[0], $l[1], $l[2]))
        $g.FillRectangle($marker, [single](40 * $s), [single]$y, [single](22 * $s), [single]$h)
        $text = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(220, 0xE2, 0xE8, 0xF0))
        $g.FillRectangle($text, [single](72 * $s), [single]$y, [single]($l[3] * $s), [single]$h)
        $y += 32 * $s
    }

    # Magnifying glass over the lower right corner.
    if ($size -ge 24) {
        $penW = [Math]::Max(2.0, 16 * $s)
        $ring = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 0xFF, 0xD5, 0x4F)), $penW
        $ring.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $ring.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
        $glass = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(110, 0x12, 0x18, 0x24))
        $cx = 160 * $s; $cy = 158 * $s; $cr = 44 * $s
        $g.FillEllipse($glass, [single]($cx - $cr), [single]($cy - $cr), [single](2 * $cr), [single](2 * $cr))
        $g.DrawEllipse($ring, [single]($cx - $cr), [single]($cy - $cr), [single](2 * $cr), [single](2 * $cr))
        $g.DrawLine($ring, [single]($cx + $cr * 0.72), [single]($cy + $cr * 0.72), [single](222 * $s), [single](220 * $s))
    }
    $g.Dispose()
    return $bmp
}

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($size in $sizes) {
    $bmp = New-IconBitmap $size
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    if ($size -eq 256) { $bmp.Save((Join-Path $assets "logview.png"), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
}

# ICO container with PNG images (supported since Windows Vista).
$out = New-Object System.IO.MemoryStream
$wr = New-Object System.IO.BinaryWriter $out
$wr.Write([UInt16]0); $wr.Write([UInt16]1); $wr.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $wr.Write([byte]$dim); $wr.Write([byte]$dim); $wr.Write([byte]0); $wr.Write([byte]0)
    $wr.Write([UInt16]1); $wr.Write([UInt16]32)
    $wr.Write([UInt32]$pngs[$i].Length); $wr.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $wr.Write($p) }
$wr.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets "logview.ico"), $out.ToArray())
"wrote $assets\logview.ico and logview.png"
