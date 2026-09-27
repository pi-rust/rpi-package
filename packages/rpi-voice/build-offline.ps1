# Build rpi-voice with the embedded offline STT (SenseVoice / sherpa-onnx).
#
# Two build-time requirements, handled here so you don't have to remember them:
#   * LIBCLANG_PATH  — bindgen (sherpa-rs-sys) needs libclang. Auto-detected from
#                      the pip `libclang` package if not already set.
#   * RUSTFLAGS      — sherpa-rs's cdylib needs the Windows system lib advapi32.
#
# Runtime needs neither: the native libs are linked statically into rpi_voice.dll.
#
# Usage (from anywhere):
#   pwsh packages/rpi-voice/build-offline.ps1

$ErrorActionPreference = "Stop"

# Run from the workspace root so `-p rpi-voice` resolves.
$root = git -C $PSScriptRoot rev-parse --show-toplevel 2>$null
if ($LASTEXITCODE -ne 0 -or -not $root) {
    $root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
Push-Location $root
try {
    # A stale LIBCLANG_PATH (e.g. from an aborted LLVM install) silently breaks
    # bindgen — validate it actually contains libclang.dll before trusting it.
    if ($env:LIBCLANG_PATH -and -not (Test-Path (Join-Path $env:LIBCLANG_PATH "libclang.dll"))) {
        Write-Host "ignoring stale LIBCLANG_PATH=$env:LIBCLANG_PATH (no libclang.dll)"
        $env:LIBCLANG_PATH = $null
    }

    if (-not $env:LIBCLANG_PATH) {
        $found = $null
        # 1) Ask whatever python/py is on PATH where its clang package lives.
        foreach ($py in @("python", "py", "python3")) {
            try {
                $native = (& $py -c "import clang,os;print(os.path.dirname(clang.__file__))" 2>$null)
                if ($native) {
                    $native = ($native | Select-Object -First 1).Trim() + "\native"
                    if (Test-Path (Join-Path $native "libclang.dll")) { $found = $native; break }
                }
            } catch { }
        }
        # 2) Glob common install layouts (fast; avoids a deep -Recurse).
        if (-not $found) {
            $patterns = @(
                "$env:ProgramFiles\LLVM\bin\libclang.dll",
                "${env:ProgramFiles(x86)}\LLVM\bin\libclang.dll",
                "$env:ChocolateyInstall\lib\llvm\tools\bin\libclang.dll",
                "$env:USERPROFILE\scoop\apps\*\current\Lib\site-packages\clang\native\libclang.dll",
                "$env:USERPROFILE\scoop\apps\*\current\lib\site-packages\clang\native\libclang.dll",
                "$env:USERPROFILE\scoop\apps\*\*\Lib\site-packages\clang\native\libclang.dll",
                "$env:USERPROFILE\scoop\apps\*\*\lib\site-packages\clang\native\libclang.dll",
                "$env:LOCALAPPDATA\Programs\Python\*\Lib\site-packages\clang\native\libclang.dll"
            )
            foreach ($p in $patterns) {
                $hit = Get-Item -Path $p -ErrorAction SilentlyContinue | Select-Object -First 1
                if ($hit) { $found = $hit.DirectoryName; break }
            }
        }
        if ($found) {
            $env:LIBCLANG_PATH = $found
            Write-Host "LIBCLANG_PATH = $env:LIBCLANG_PATH"
        }
    }
    if (-not $env:LIBCLANG_PATH) {
        throw "libclang not found. Install it (`pip install libclang`) and set LIBCLANG_PATH to the dir containing libclang.dll (e.g. <python>\Lib\site-packages\clang\native)."
    }

    if (-not $env:RUSTFLAGS) { $env:RUSTFLAGS = "" }
    if ($env:RUSTFLAGS -notmatch "advapi32") {
        $env:RUSTFLAGS = ("$($env:RUSTFLAGS) -C link-arg=advapi32.lib").Trim()
    }
    Write-Host "RUSTFLAGS     = $env:RUSTFLAGS"

    cargo build -p rpi-voice --release --features local-stt
} finally {
    Pop-Location
}
