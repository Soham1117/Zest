#requires -Version 5.1
<#
  Recreates tests\fixtures\* with the tools in third_party\bin.
  Run tools\fetch-deps.ps1 first. Usage: powershell -ExecutionPolicy Bypass -File tools\make-fixtures.ps1
#>
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$vips = Join-Path $root 'third_party\bin\vips\bin\vips.exe'
$ff   = Join-Path $root 'third_party\bin\ffmpeg\bin\ffmpeg.exe'
$fx   = Join-Path $root 'tests\fixtures'
foreach ($t in $vips, $ff) { if (-not (Test-Path $t)) { Write-Host "Missing $t. Run tools\fetch-deps.ps1 first."; exit 1 } }
New-Item -ItemType Directory -Force $fx | Out-Null
$script:skipped = @()
$script:failed = 0

function Run($exe, [string[]]$a, $out) {
    & $exe @a
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $out)) { Write-Host "FAILED: $out" -ForegroundColor Red; $script:failed++; return $false }
    Write-Host ("ok  {0} ({1:N1} KB)" -f (Split-Path $out -Leaf), ((Get-Item $out).Length / 1KB)); return $true
}
function FF([string[]]$a, $name) { $o = Join-Path $fx $name; Run $ff (@('-hide_banner','-loglevel','error','-y') + $a + $o) $o | Out-Null }
function VIPS($src, $name, $suffix = '') { $o = Join-Path $fx $name; Run $vips @('copy', (Join-Path $fx $src), ($o + $suffix)) $o | Out-Null }

# --- images (source: ffmpeg testsrc2, 320x240) ---
FF @('-f','lavfi','-i','testsrc2=size=320x240:rate=1','-frames:v','1') 'sample.png'
VIPS 'sample.png' 'sample.jpg'  '[Q=85]'
VIPS 'sample.png' 'sample.webp' '[Q=80]'
VIPS 'sample.png' 'sample.tiff' '[compression=deflate]'
VIPS 'sample.png' 'sample.avif' '[Q=50,compression=av1]'
FF @('-i',(Join-Path $fx 'sample.png'),'-pix_fmt','bgr24') 'sample.bmp'
FF @('-f','lavfi','-i','testsrc2=size=320x240:rate=10:duration=1','-vf','scale=160:120,split[a][b];[a]palettegen[p];[b][p]paletteuse') 'sample.gif'
$heic = Join-Path $fx 'sample.heic'
& $vips copy (Join-Path $fx 'sample.png') ($heic + '[Q=50,compression=hevc]')
if ($LASTEXITCODE -eq 0 -and (Test-Path $heic)) { Write-Host ("ok  sample.heic ({0:N1} KB)" -f ((Get-Item $heic).Length / 1KB)) }
else { Write-Host 'SKIPPED sample.heic (vips build cannot encode HEVC)'; $script:skipped += 'sample.heic'; if (Test-Path $heic) { Remove-Item $heic } }

# --- video (2 s, 320x240, testsrc + sine audio) ---
$vin = @('-f','lavfi','-i','testsrc=size=320x240:rate=25:duration=2','-f','lavfi','-i','sine=frequency=440:duration=2')
FF ($vin + @('-c:v','libx264','-preset','veryfast','-crf','28','-pix_fmt','yuv420p','-c:a','aac','-b:a','64k','-movflags','+faststart')) 'sample.mp4'
FF ($vin + @('-c:v','libx264','-preset','veryfast','-crf','28','-pix_fmt','yuv420p','-c:a','aac','-b:a','64k')) 'sample.mov'
FF ($vin + @('-c:v','libx264','-preset','veryfast','-crf','28','-pix_fmt','yuv420p','-c:a','aac','-b:a','64k')) 'sample.mkv'
FF ($vin + @('-c:v','libvpx-vp9','-b:v','0','-crf','40','-deadline','good','-cpu-used','5','-c:a','libopus','-b:a','48k')) 'sample.webm'
FF ($vin + @('-c:v','mpeg4','-q:v','8','-c:a','libmp3lame','-b:a','64k')) 'sample.avi'

# --- audio (1 s sine) ---
$ain = @('-f','lavfi','-i','sine=frequency=440:duration=1')
FF ($ain + @('-c:a','libmp3lame','-b:a','96k')) 'sample.mp3'
FF ($ain + @('-c:a','pcm_s16le','-ar','22050')) 'sample.wav'
FF ($ain + @('-c:a','flac')) 'sample.flac'
FF ($ain + @('-c:a','aac','-b:a','64k')) 'sample.m4a'
FF ($ain + @('-c:a','libvorbis','-q:a','3')) 'sample.ogg'
FF ($ain + @('-c:a','libopus','-b:a','48k')) 'sample.opus'

# --- signature-test files ---
# corrupt.jpg: valid JPEG start (SOI + JFIF APP0) then garbage
$hdr = [byte[]](0xFF,0xD8,0xFF,0xE0,0x00,0x10,0x4A,0x46,0x49,0x46,0x00,0x01,0x01,0x00,0x00,0x01,0x00,0x01,0x00,0x00)
$rng = New-Object Random 12345
$junk = New-Object byte[] 2048; $rng.NextBytes($junk)
[IO.File]::WriteAllBytes((Join-Path $fx 'corrupt.jpg'), ($hdr + $junk))
Write-Host 'ok  corrupt.jpg'
# fake.png: a real JPEG with a .png name
Copy-Item (Join-Path $fx 'sample.jpg') (Join-Path $fx 'fake.png') -Force
Write-Host 'ok  fake.png'

$big = Get-ChildItem $fx -File | Where-Object { $_.Length -ge 300KB }
foreach ($b in $big) { Write-Host "WARNING: $($b.Name) is over 300 KB" -ForegroundColor Yellow }
if ($script:skipped.Count) { Write-Host ("Skipped: " + ($script:skipped -join ', ')) }
if ($script:failed) { exit 1 }
exit 0
