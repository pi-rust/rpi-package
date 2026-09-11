$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $workspace "dist"
$stage = Join-Path $dist "rpi-packages-windows-x86_64"
$archive = Join-Path $dist "rpi-packages-windows-x86_64.zip"

New-Item -ItemType Directory -Force -Path $stage | Out-Null
& (Join-Path $PSScriptRoot "install.ps1") -TargetDir $stage
Copy-Item -LiteralPath (Join-Path $workspace "catalog\packages.json") -Destination $stage -Force
Copy-Item -LiteralPath (Join-Path $workspace "README.md") -Destination $stage -Force
Copy-Item -LiteralPath (Join-Path $workspace "scripts\install.ps1") -Destination $stage -Force
Copy-Item -LiteralPath (Join-Path $workspace "scripts\install-bundle.ps1") -Destination $stage -Force
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -Force
Write-Host "Created $archive"
