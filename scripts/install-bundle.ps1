param(
    [Parameter(Mandatory = $true)]
    [string]$Archive,
    [string]$TargetDir = ""
)

$ErrorActionPreference = "Stop"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("rpi-packages-" + [guid]::NewGuid().ToString("N"))
$zipPath = Join-Path $tempRoot "packages.zip"
$extractDir = Join-Path $tempRoot "extract"
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
try {
    if ($Archive -match '^https?://') {
        Invoke-WebRequest -UseBasicParsing -Uri $Archive -OutFile $zipPath
    } else {
        Copy-Item -LiteralPath ([System.IO.Path]::GetFullPath($Archive)) -Destination $zipPath
    }
    Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force
    & (Join-Path $PSScriptRoot "install.ps1") -TargetDir $TargetDir -SourceDir $extractDir
} finally {
    if (Test-Path -LiteralPath $tempRoot) { Remove-Item -LiteralPath $tempRoot -Recurse -Force }
}
