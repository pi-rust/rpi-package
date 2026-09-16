param(
    [string]$TargetDir = "",
    [string]$SourceDir = ""
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$release = if ([string]::IsNullOrWhiteSpace($SourceDir)) { Join-Path $workspace "target\release" } else { [System.IO.Path]::GetFullPath($SourceDir) }
if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    if (-not [string]::IsNullOrWhiteSpace($env:RPI_CODING_AGENT_DIR)) {
        $TargetDir = Join-Path $env:RPI_CODING_AGENT_DIR "extensions"
    } else {
        $TargetDir = Join-Path $env:USERPROFILE ".rpi\agent\extensions"
    }
}
$resolvedTarget = [System.IO.Path]::GetFullPath($TargetDir)
New-Item -ItemType Directory -Force -Path $resolvedTarget | Out-Null

$names = @(
    "rpi_mcp_adapter",
    "rpi_web_access",
    "rpi_subagents",
    "rpi_background_tasks",
    "rpi_lens",
    "rpi_todo",
    "rpi_codegraph",
    "rpi_memory",
    "rpi_token_usage",
    "rpi_ask_user",
    "rpi_permissions",
    "rpi_simplify",
    "rpi_search",
    "rpi_goal",
    "rpi_websearch",
    "rpi_webfetch",
    "rpi_firecrawl",
    "rpi_extension_rpc",
    "rpi_im_message"
)

foreach ($name in $names) {
    $source = Join-Path $release "$name.dll"
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Missing package artifact: $source"
    }
    Copy-Item -LiteralPath $source -Destination $resolvedTarget -Force
}

Write-Host "Installed 19 rpi packages to $resolvedTarget"
