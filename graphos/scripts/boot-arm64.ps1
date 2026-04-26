#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS AArch64 QEMU boot helper.
#
# Builds the AArch64 kernel image and boots it on QEMU `virt`, capturing
# serial logs for quick Phase F smoke validation.
#
# Usage:
#   .\scripts\boot-arm64.ps1
#   .\scripts\boot-arm64.ps1 -NoBuild -TimeoutSec 45
#
# Exit codes:
#   0 = observed expected boot markers
#   1 = build failure, runtime failure, or timeout

param(
    [switch]$NoBuild,
    [int]$TimeoutSec = 30,
    [string]$SerialLog = "target\boot-arm64-serial.txt"
)

$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\qemu;$env:PATH"

$WORKSPACE = $PSScriptRoot | Split-Path
Set-Location $WORKSPACE

function Fail([string]$msg) {
    Write-Host "[boot-arm64] FAIL: $msg" -ForegroundColor Red
    exit 1
}

function Info([string]$msg) {
    Write-Host "[boot-arm64] $msg" -ForegroundColor Cyan
}

$KernelElf = "target\aarch64-unknown-none\debug\graphos-kernel"

if (-not $NoBuild) {
    Info "building aarch64 kernel"
    & cargo build -p graphos-kernel --target aarch64-unknown-none "-Zbuild-std=core,compiler_builtins,alloc" --features freestanding
    if ($LASTEXITCODE -ne 0) { Fail "aarch64 kernel build failed" }
}

if (-not (Test-Path $KernelElf)) {
    Fail "missing kernel artifact: $KernelElf"
}

$qemu = Get-Command qemu-system-aarch64 -ErrorAction SilentlyContinue
if (-not $qemu) {
    Fail "qemu-system-aarch64 not found in PATH"
}

Remove-Item $SerialLog -ErrorAction SilentlyContinue

$QemuArgs = @(
    "-machine", "virt",
    "-cpu", "cortex-a72",
    "-m", "512M",
    "-kernel", (Resolve-Path $KernelElf).Path,
    "-serial", "file:$SerialLog",
    "-display", "none",
    "-monitor", "none",
    "-no-reboot"
)

Info "starting qemu-system-aarch64"
$proc = Start-Process $qemu.Source -ArgumentList $QemuArgs -PassThru
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$matched = $false

try {
    while ((Get-Date) -lt $deadline) {
        if ($proc.HasExited) {
            break
        }
        Start-Sleep -Milliseconds 250
        if (Test-Path $SerialLog) {
            $log = Get-Content $SerialLog -Raw -ErrorAction SilentlyContinue
            if ($log -match "\[boot\] GraphOS AArch64 kernel|\[boot\] entering idle|health=ready") {
                $matched = $true
                break
            }
        }
    }
}
finally {
    if ($proc -and -not $proc.HasExited) {
        $proc.Kill() 2>$null
    }
}

if (-not (Test-Path $SerialLog)) {
    Fail "serial log not created"
}

$tail = Get-Content $SerialLog -ErrorAction SilentlyContinue | Select-Object -Last 25
if ($matched) {
    Info "observed expected ARM64 boot marker"
    if ($tail) {
        Write-Host "[boot-arm64] serial tail:" -ForegroundColor DarkCyan
        $tail | ForEach-Object { Write-Host $_ }
    }
    exit 0
}

if ($tail) {
    Write-Host "[boot-arm64] serial tail:" -ForegroundColor Yellow
    $tail | ForEach-Object { Write-Host $_ }
}

Fail "timed out waiting for ARM64 boot marker"
