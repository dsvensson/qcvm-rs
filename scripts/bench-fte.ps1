# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Times qcvm against FTE's standalone runner on the workloads in tests/qc/bench.qc.
#
# Each workload is compiled at two sizes and both runners run each size several times,
# interleaved; the minimum wall-clock time per runner and size is kept. The cost of one
# iteration is the difference between the two sizes divided by the difference in iterations, so
# process start-up and program loading cancel out. (The loops count in floats, so they stay
# below 2^24, where i++ stops counting.)
#
# Usage:
#   scripts/bench-fte.ps1 [-Runs 7] [-Workloads loop,fields] [-Ours <run.exe>] [-NoBuild]
#
# Needs fteqcc (fteqcc64/fteqcc on PATH, or $env:FTEQCC) and FTE's runner ($env:FTE_QCVM), see
# scripts/build-fte-tools.ps1. Builds `cargo build --release --example run` unless -NoBuild or
# -Ours is given; set RUSTFLAGS or CARGO_PROFILE_RELEASE_* beforehand to compare build settings.

param(
    [int]$Runs = 7,
    [string[]]$Workloads = @("fib", "loop", "fields", "fields_add"),
    [string]$Ours = "",
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Find-Tool([string[]]$names, [string]$envVar) {
    foreach ($name in $names) {
        $cmd = Get-Command $name -ErrorAction SilentlyContinue
        if ($cmd) { return $cmd.Source }
    }
    $path = [Environment]::GetEnvironmentVariable($envVar)
    if ($path -and (Test-Path $path)) { return (Resolve-Path $path).Path }
    return $null
}

$fteqcc = Find-Tool @("fteqcc64", "fteqcc") "FTEQCC"
if (-not $fteqcc) { throw "fteqcc not found: put it on PATH or set FTEQCC" }
$fte = Find-Tool @() "FTE_QCVM"
if (-not $fte) { throw "FTE's runner not found: set FTE_QCVM" }

if (-not $Ours) {
    if (-not $NoBuild) {
        cargo build --release --example run --manifest-path (Join-Path $root "Cargo.toml")
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    }
    $Ours = Join-Path $root "target\release\examples\run.exe"
}

# Workload: define, the two sizes, iterations for a size, statements per iteration (0: varies).
function Fib-Calls([int]$n) {
    # fib(n) makes 2 * F(n + 1) - 1 calls.
    $a = 0; $b = 1
    for ($i = 0; $i -lt $n + 1; $i++) { $t = $a + $b; $a = $b; $b = $t }
    return 2 * $a - 1
}
$all = @{
    fib        = @{ Define = "FIB"; Sizes = @(24, 30); Iterations = { param($n) Fib-Calls $n }; Statements = 0; Unit = "call" }
    loop       = @{ Define = "LOOP"; Sizes = @(2000000, 16000000); Iterations = { param($n) $n }; Statements = 6; Unit = "iteration" }
    fields     = @{ Define = "FIELDS"; Sizes = @(2000000, 16000000); Iterations = { param($n) $n }; Statements = 8; Unit = "iteration" }
    fields_add = @{ Define = "FIELDS_ADD"; Sizes = @(2000000, 16000000); Iterations = { param($n) $n }; Statements = 7; Unit = "iteration" }
}

$work = Join-Path $root "target\bench-fte"
New-Item -ItemType Directory -Force $work | Out-Null

function Compile([string]$define, [int]$n) {
    $dir = Join-Path $work "$define-$n"
    New-Item -ItemType Directory -Force $dir | Out-Null
    foreach ($f in @("runner.qh", "bench.qc")) {
        Copy-Item (Join-Path $root "tests\qc\$f") $dir -Force
    }
    Set-Content -Path (Join-Path $dir "progs.src") -Value "bench.dat`nrunner.qh`nbench.qc" -NoNewline
    Push-Location $dir
    try {
        $log = & $fteqcc -srcfile progs.src -Tfte -O3 "-D$define" "-DN=$n" 2>&1
        if (-not (Test-Path "bench.dat")) { throw "fteqcc failed:`n$log" }
    } finally { Pop-Location }
    return (Join-Path $dir "bench.dat")
}

function Fmt([int]$decimals, [double]$v) {
    return $v.ToString("F$decimals", [cultureinfo]::InvariantCulture)
}

function Time-Run([string]$exe, [string]$dat) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $out = & $exe $dat 2>$null
    $sw.Stop()
    return @{ Ms = $sw.Elapsed.TotalMilliseconds; Out = ($out -join "`n") }
}

$rows = @()
foreach ($name in $Workloads) {
    $w = $all[$name]
    if (-not $w) { throw "unknown workload '$name' (known: $($all.Keys -join ', '))" }
    $dats = $w.Sizes | ForEach-Object { Compile $w.Define $_ }
    $best = @{}
    foreach ($r in 1..$Runs) {
        foreach ($i in 0..1) {
            foreach ($runner in @("fte", "ours")) {
                $exe = if ($runner -eq "fte") { $fte } else { $Ours }
                $t = Time-Run $exe $dats[$i]
                $key = "$runner$i"
                if (-not $best.ContainsKey($key) -or $t.Ms -lt $best[$key].Ms) { $best[$key] = $t }
            }
        }
    }
    foreach ($i in 0..1) {
        if ($best["fte$i"].Out -ne $best["ours$i"].Out) {
            throw "$name size $($w.Sizes[$i]): outputs differ: FTE '$($best["fte$i"].Out)', ours '$($best["ours$i"].Out)'"
        }
    }
    $iters = (& $w.Iterations $w.Sizes[1]) - (& $w.Iterations $w.Sizes[0])
    $fteNs = ($best["fte1"].Ms - $best["fte0"].Ms) * 1e6 / $iters
    $oursNs = ($best["ours1"].Ms - $best["ours0"].Ms) * 1e6 / $iters
    $perStmt = { param($ns) if ($w.Statements) { Fmt 3 ($ns / $w.Statements) } else { "" } }
    $row = [pscustomobject]@{
        Workload       = $name
        Per            = $w.Unit
        "FTE ns"       = Fmt 2 $fteNs
        "qcvm ns"      = Fmt 2 $oursNs
        Ratio          = Fmt 2 ($oursNs / $fteNs)
        "FTE ns/stmt"  = & $perStmt $fteNs
        "qcvm ns/stmt" = & $perStmt $oursNs
    }
    $rows += $row
}
($rows | Format-Table -AutoSize | Out-String).TrimEnd()
