# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#Requires -Version 5.1
<#
.SYNOPSIS
    GraphOS performance benchmark runner.

.DESCRIPTION
    Runs `cargo test -p graphos-kernel` in release mode and extracts
    timing results from test output.  Compares them against a stored
    baseline in docs/qa/bench-baseline.txt.

    Fails (exit 1) if any benchmark category regresses more than 5 %
    versus the baseline.  Creates the baseline on first run.

    Benchmark output expected format (from #[test] functions):
        BENCH <name> <nanoseconds_per_iter>
    e.g.:
        BENCH ipc_roundtrip 1240
        BENCH syscall_overhead 320
        BENCH graph_arena_insert 88
        BENCH graph_arena_lookup 42
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot   = Split-Path -Parent $PSScriptRoot
$BaselineFile = Join-Path (Join-Path (Join-Path $RepoRoot 'docs') 'qa') 'bench-baseline.txt'
$Threshold  = 0.05   # 5 % regression threshold
$Passed     = $true

function Write-Check {
    param([string]$Label, [bool]$Ok, [string]$Detail = '')
    $status = if ($Ok) { 'PASS' } else { 'FAIL' }
    $colour = if ($Ok) { 'Green' } else { 'Red' }
    Write-Host "[$status] $Label" -ForegroundColor $colour
    if ($Detail) { Write-Host "       $Detail" -ForegroundColor $(if ($Ok) { 'Cyan' } else { 'Yellow' }) }
}

# ── Run tests ──────────────────────────────────────────────────────────────────

Write-Host 'Running cargo test (release) ...' -ForegroundColor Cyan
Push-Location $RepoRoot
try {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $raw = & cargo test -p graphos-kernel --release -- --nocapture 2>&1
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $prev
    }
} finally {
    Pop-Location
}

if ($exitCode -ne 0) {
    Write-Check 'cargo test -p graphos-kernel --release' $false "exit code $exitCode"
    $raw | Select-Object -Last 30 | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Check 'cargo test -p graphos-kernel --release' $true

# ── Extract BENCH lines ────────────────────────────────────────────────────────

$results = [ordered]@{}
foreach ($line in $raw) {
    if ($line -match '^BENCH\s+(\S+)\s+(\d+)') {
        $results[$Matches[1]] = [long]$Matches[2]
    }
}

if ($results.Count -eq 0) {
    Write-Host '[INFO] No BENCH output found; no regression check performed.' -ForegroundColor Cyan
    exit 0
}

Write-Host "`nBenchmark results:" -ForegroundColor Cyan
foreach ($kv in $results.GetEnumerator()) {
    Write-Host ("  {0,-35} {1,8} ns/iter" -f $kv.Key, $kv.Value)
}

# ── Baseline comparison ────────────────────────────────────────────────────────

if (Test-Path $BaselineFile) {
    $baseline = [ordered]@{}
    foreach ($line in (Get-Content $BaselineFile)) {
        if ($line -match '^(\S+)\s+(\d+)') {
            $baseline[$Matches[1]] = [long]$Matches[2]
        }
    }

    Write-Host "`nRegression check (threshold: $([int]($Threshold*100))%):" -ForegroundColor Cyan
    foreach ($kv in $results.GetEnumerator()) {
        $name    = $kv.Key
        $current = $kv.Value
        if ($baseline.Contains($name)) {
            $base     = $baseline[$name]
            $delta    = ($current - $base) / [double]$base
            $regress  = $delta -gt $Threshold
            $detail   = "baseline=$base  current=$current  delta=$('{0:+0.1%}' -f $delta)"
            Write-Check "  $name" (-not $regress) $detail
            if ($regress) { $Passed = $false }
        } else {
            Write-Host "  [NEW ] $name = $current ns/iter" -ForegroundColor Cyan
        }
    }
} else {
    # First run — write baseline.
    $null = New-Item -ItemType Directory -Force -Path (Split-Path $BaselineFile)
    $lines = $results.GetEnumerator() | ForEach-Object { "$($_.Key) $($_.Value)" }
    $lines | Set-Content $BaselineFile
    Write-Check 'Baseline created (first run)' $true $BaselineFile
}

# ── Result ─────────────────────────────────────────────────────────────────────

if ($Passed) {
    Write-Host "`nAll benchmark checks passed." -ForegroundColor Green
    exit 0
} else {
    Write-Host "`nBenchmark regression(s) detected." -ForegroundColor Red
    exit 1
}
