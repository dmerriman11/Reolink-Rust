# Builds the release exe plus NSIS and MSI installers into dist/.
# Bundles the ffmpeg.exe found on PATH (the app decodes and records with it).
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ffmpeg) { throw "ffmpeg.exe not found on PATH; install it (winget install Gyan.FFmpeg) and retry." }
$ffmpeg = (Get-Item $ffmpeg).Target ?? $ffmpeg   # resolve WinGet's symlink
New-Item -ItemType Directory -Force bundle, dist | Out-Null
Copy-Item $ffmpeg bundle\ffmpeg.exe -Force
$license = Join-Path (Split-Path (Split-Path $ffmpeg)) "LICENSE"
if (Test-Path $license) { Copy-Item $license bundle\FFMPEG-LICENSE.txt -Force }
else { Set-Content bundle\FFMPEG-LICENSE.txt "FFmpeg is licensed under the GPL/LGPL; see https://ffmpeg.org/legal.html" }

npx --yes @tauri-apps/cli@2.12.4 build
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }

$version = (Get-Content tauri.conf.json | ConvertFrom-Json).version
Copy-Item target\release\reolink-rs.exe "dist\Reolink-rs_$version.exe" -Force
Copy-Item target\release\bundle\nsis\*.exe dist\ -Force
Copy-Item target\release\bundle\msi\*.msi dist\ -Force
Get-ChildItem dist | Format-Table Name, @{n="MB"; e={[math]::Round($_.Length / 1MB, 1)}}
