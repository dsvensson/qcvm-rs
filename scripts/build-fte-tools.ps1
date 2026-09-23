# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Builds FTE's command-line QuakeC compiler (fteqcc) and its standalone QuakeC
# runner (qcvm) out of tree. Both are GPL programs; this crate only ever runs
# them as black-box tools (compiling test fixtures, differential testing) and
# never links against or redistributes them.
#
# Usage:
#   scripts/build-fte-tools.ps1 [-FteSource <path>] [-BuildDir <path>]
#
# Afterwards point the test suite at the results:
#   $env:FTEQCC   = "<BuildDir>\fteqcc.exe"
#   $env:FTE_QCVM = "<BuildDir>\qcvm.exe"

param(
    [string]$FteSource = (Join-Path $PSScriptRoot "..\..\..\CLionProjects\fteqw-clean"),
    [string]$BuildDir = (Join-Path $PSScriptRoot "..\..\..\CLionProjects\fte-tools-build"),
    [string]$Generator = "Ninja",
    [string]$CCompiler = "gcc"
)

$ErrorActionPreference = "Stop"

$FteSource = (Resolve-Path $FteSource).Path
if (-not (Test-Path (Join-Path $FteSource "CMakeLists.txt"))) {
    throw "No CMakeLists.txt in FTE source directory '$FteSource'"
}
New-Item -ItemType Directory -Force $BuildDir | Out-Null
$BuildDir = (Resolve-Path $BuildDir).Path

# A cache from a previous run may pin a different compiler; start clean.
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $BuildDir "CMakeCache.txt")

# Only the two tools are wanted: switch the engine, plugins and the other tools
# off so configuration does not need their third-party dependencies.
$options = @(
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_C_COMPILER=$CCompiler",
    # FTE bakes the install prefix into a compile definition unquoted, so it must not contain
    # spaces (the Windows default "Program Files (x86)" breaks the build).
    "-DCMAKE_INSTALL_PREFIX=$($BuildDir.Replace('\', '/'))/install",
    "-DFTE_ENGINE=OFF",
    "-DFTE_ENGINE_SERVER_ONLY=OFF",
    "-DFTE_MENU_SYS=OFF",
    "-DFTE_CSADDON=OFF",
    "-DFTE_TOOL_QCC=ON",
    "-DFTE_TOOL_QCVM=ON",
    "-DFTE_TOOL_QCCGUI=OFF",
    "-DFTE_TOOL_IQM=OFF",
    "-DFTE_TOOL_IMAGE=OFF",
    "-DFTE_TOOL_QTV=OFF",
    "-DFTE_TOOL_MASTER=OFF",
    "-DFTE_TOOL_HTTPSV=OFF",
    "-DFTE_PLUG_BULLET=OFF", "-DFTE_PLUG_CEF=OFF", "-DFTE_PLUG_COD=OFF", "-DFTE_PLUG_EZHUD=OFF",
    "-DFTE_PLUG_FFMPEG=OFF", "-DFTE_PLUG_GNUTLS=OFF", "-DFTE_PLUG_HL2=OFF", "-DFTE_PLUG_IRC=OFF",
    "-DFTE_PLUG_MODELS=OFF", "-DFTE_PLUG_MPQ=OFF", "-DFTE_PLUG_NAMEMAKER=OFF", "-DFTE_PLUG_ODE=OFF",
    "-DFTE_PLUG_OPENSSL=OFF", "-DFTE_PLUG_OPENXR=OFF", "-DFTE_PLUG_QI=OFF", "-DFTE_PLUG_QUAKE3=OFF",
    "-DFTE_PLUG_TERRAINGEN=OFF", "-DFTE_PLUG_TIMIDITY=OFF", "-DFTE_PLUG_X11SV=OFF", "-DFTE_PLUG_XMPP=OFF"
)

cmake -S $FteSource -B $BuildDir -G $Generator @options
if ($LASTEXITCODE -ne 0) { throw "CMake configure failed" }

cmake --build $BuildDir --target fteqcc qcvm
if ($LASTEXITCODE -ne 0) { throw "CMake build failed" }

Write-Host ""
Write-Host "Built tools in $BuildDir"
Get-ChildItem $BuildDir -Filter *.exe | Where-Object { $_.Name -match '^(fteqcc|qcvm)' } |
    ForEach-Object { Write-Host "  $($_.FullName)" }
