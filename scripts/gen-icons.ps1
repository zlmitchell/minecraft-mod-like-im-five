# Generates placeholder RGBA icons for Tauri so `npx tauri dev` can compile
# without requiring ImageMagick or a real source asset. Run once.
#
#   pwsh ./scripts/gen-icons.ps1
#
# Replace later with: npx @tauri-apps/cli icon path/to/source.png

Add-Type -AssemblyName System.Drawing

$root = Join-Path $PSScriptRoot ".."
$iconDir = Join-Path $root "src-tauri/icons"
New-Item -ItemType Directory -Force -Path $iconDir | Out-Null

$color = [System.Drawing.Color]::FromArgb(255, 124, 77, 255)  # #7c4dff

function New-PlaceholderPng($path, $w, $h) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear($color)
    $g.Dispose()
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "  wrote $path"
}

New-PlaceholderPng (Join-Path $iconDir "32x32.png")     32  32
New-PlaceholderPng (Join-Path $iconDir "128x128.png")  128 128
New-PlaceholderPng (Join-Path $iconDir "128x128@2x.png") 256 256
New-PlaceholderPng (Join-Path $iconDir "icon.png")    1024 1024

# .ico — Tauri accepts a PNG-with-ICO-header for dev. Real ICO needs a
# multi-resolution conversion; for dev a simple wrapper is fine.
$pngBytes = [System.IO.File]::ReadAllBytes((Join-Path $iconDir "32x32.png"))
$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $ms
# ICONDIR
$bw.Write([uint16]0)        # reserved
$bw.Write([uint16]1)        # type=icon
$bw.Write([uint16]1)        # count
# ICONDIRENTRY
$bw.Write([byte]32)         # width
$bw.Write([byte]32)         # height
$bw.Write([byte]0)           # palette
$bw.Write([byte]0)           # reserved
$bw.Write([uint16]1)        # color planes
$bw.Write([uint16]32)       # bpp
$bw.Write([uint32]$pngBytes.Length)  # size
$bw.Write([uint32]22)       # offset (after this 22-byte header)
$bw.Write($pngBytes)
[System.IO.File]::WriteAllBytes((Join-Path $iconDir "icon.ico"), $ms.ToArray())
Write-Host "  wrote $iconDir/icon.ico"

# .icns — macOS only. Tauri proc-macros validate it exists; a copy of the
# 128px PNG is good enough for cross-compile checks. Real .icns needed for
# Mac bundle, generated with `npx tauri icon` or `iconutil`.
Copy-Item (Join-Path $iconDir "128x128.png") (Join-Path $iconDir "icon.icns") -Force
Write-Host "  wrote $iconDir/icon.icns (placeholder)"

Write-Host ""
Write-Host "Done. Replace later with `npx @tauri-apps/cli icon assets/source.png`."
