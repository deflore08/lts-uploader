$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$image = "lts-uploader-linux-builder"

docker build -f "$root\scripts\Dockerfile.linux" -t $image "$root\scripts"

New-Item -ItemType Directory -Force -Path "$root\dist" | Out-Null

docker run --rm `
  -v "${root}:/app" `
  -w /app `
  $image `
  bash -lc 'set -euo pipefail; npm ci; npm run build:linux; cp -v src-tauri/target/release/bundle/appimage/*.AppImage dist/'

Get-ChildItem "$root\dist\*.AppImage"