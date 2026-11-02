param(
    [ValidateSet('debug','release','unsigned-release')][string]$Variant = 'debug',
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
$argsList = @((Join-Path $PSScriptRoot 'tools/build.py'), '--variant', $Variant)
if ($Offline) { $argsList += '--offline' }
& python @argsList
exit $LASTEXITCODE
