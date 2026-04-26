# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#Requires -Version 5.1
<#
.SYNOPSIS
    GraphOS SDK validation script.

.DESCRIPTION
    Checks that:
      1. Each SDK crate has a src/ directory with at least one .rs file.
      2. Each SDK crate passes `cargo check`.
      3. Syscall numbers in kernel/src/syscall/ match a stored baseline
         (docs/qa/syscall-baseline.txt) when the baseline file exists.

    Exits with 0 if all checks pass, 1 otherwise.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$SdkRoot   = Join-Path $RepoRoot 'sdk'
$Workspace = Join-Path $RepoRoot 'Cargo.toml'
$Baseline  = Join-Path $RepoRoot 'docs' 'qa' 'syscall-baseline.txt'
$Passed    = $true

function Write-Check {
    param([string]$Label, [bool]$Ok, [string]$Detail = '')
    $status = if ($Ok) { 'PASS' } else { 'FAIL' }
    $colour = if ($Ok) { 'Green' } else { 'Red' }
    Write-Host "[$status] $Label" -ForegroundColor $colour
    if (-not $Ok -and $Detail) {
        Write-Host "       $Detail" -ForegroundColor Yellow
    }
}

# ── 1. SDK src/ presence check ─────────────────────────────────────────────────

$SdkCrates = @('app-sdk', 'graph-sdk', 'tool-sdk', 'wasm-sdk')
foreach ($crate in $SdkCrates) {
    $src = Join-Path $SdkRoot $crate 'src'
    $hasFiles = (Test-Path $src) -and ((Get-ChildItem $src -Filter '*.rs' -Recurse -ErrorAction SilentlyContinue | Measure-Object).Count -gt 0)
    Write-Check "SDK $crate src/ exists" $hasFiles
    if (-not $hasFiles) { $Passed = $false }
}

# ── 2. cargo check for each SDK crate ─────────────────────────────────────────

Push-Location $RepoRoot
try {
    $cargoCrates = @(
        @{ Pkg = 'graphos-app-sdk';  Dir = 'app-sdk'   }
        @{ Pkg = 'graphos-graph-sdk'; Dir = 'graph-sdk' }
        @{ Pkg = 'graphos-tool-sdk'; Dir = 'tool-sdk'  }
    )
    foreach ($entry in $cargoCrates) {
        $manifestPath = Join-Path $SdkRoot $entry.Dir 'Cargo.toml'
        if (-not (Test-Path $manifestPath)) {
            Write-Check "cargo check $($entry.Pkg)" $false "Cargo.toml not found at $manifestPath"
            $Passed = $false
            continue
        }
        $result = & cargo check --manifest-path $manifestPath --message-format short 2>&1
        $ok = $LASTEXITCODE -eq 0
        Write-Check "cargo check $($entry.Pkg)" $ok
        if (-not $ok) {
            $Passed = $false
            $result | Select-Object -First 10 | ForEach-Object { Write-Host "       $_" -ForegroundColor Yellow }
        }
    }
}
finally {
    Pop-Location
}

# ── 3. Syscall number ABI stability check ─────────────────────────────────────

$SyscallDir = Join-Path $RepoRoot 'kernel' 'src' 'syscall'
if (Test-Path $SyscallDir) {
    # Extract lines like: pub const SYS_XXX: usize = NNN;
    $current = Get-ChildItem $SyscallDir -Filter '*.rs' -Recurse |
        Select-String -Pattern 'pub\s+const\s+SYS_\w+\s*:\s*\w+\s*=\s*\d+' |
        ForEach-Object { $_.Line.Trim() } |
        Sort-Object

    if (Test-Path $Baseline) {
        $baseline = Get-Content $Baseline | Where-Object { $_ -match '\S' } | Sort-Object
        $added   = Compare-Object $baseline $current -PassThru | Where-Object { $_.SideIndicator -eq '=>' }
        $removed = Compare-Object $baseline $current -PassThru | Where-Object { $_.SideIndicator -eq '<=' }

        if ($removed) {
            Write-Check 'Syscall ABI stability (no removed constants)' $false
            $removed | ForEach-Object { Write-Host "       REMOVED: $_" -ForegroundColor Red }
            $Passed = $false
        } else {
            Write-Check 'Syscall ABI stability (no removed constants)' $true
        }

        if ($added) {
            Write-Host '[INFO] New syscall constants (additions are OK):' -ForegroundColor Cyan
            $added | ForEach-Object { Write-Host "       + $_" -ForegroundColor Cyan }
        }
    } else {
        # No baseline yet — generate one.
        $null = New-Item -ItemType Directory -Force -Path (Split-Path $Baseline)
        $current | Set-Content $Baseline
        Write-Check 'Syscall baseline created (first run)' $true
    }
} else {
    Write-Check 'Syscall directory found' $false "Expected: $SyscallDir"
    $Passed = $false
}

# ── Result ─────────────────────────────────────────────────────────────────────

if ($Passed) {
    Write-Host "`nAll SDK checks passed." -ForegroundColor Green
    exit 0
} else {
    Write-Host "`nOne or more SDK checks failed." -ForegroundColor Red
    exit 1
}
