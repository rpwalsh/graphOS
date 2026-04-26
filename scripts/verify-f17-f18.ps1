#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS Phase F validation harness (Sessions 17-18).
#
# F-17 gates:
#  - aarch64 kernel compile check
#  - AArch64 QEMU boot marker check
#
# F-18 gates:
#  - x86_64 kernel compile check
#  - NVMe write path implemented
#  - Wi-Fi/Bluetooth driver probe hooks wired into builtins

param(
    [int]$Arm64TimeoutSec = 35,
    [switch]$SkipArmBoot
)

$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\qemu;$env:PATH"

$WORKSPACE = $PSScriptRoot | Split-Path
Set-Location $WORKSPACE

$Failures = [System.Collections.Generic.List[string]]::new()

function Pass([string]$label) {
    Write-Host "[PASS] $label" -ForegroundColor Green
}

function Fail([string]$label, [string]$detail) {
    Write-Host "[FAIL] $label : $detail" -ForegroundColor Red
    $Failures.Add("$label : $detail")
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @()
    )

    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $FilePath @Arguments 2>&1
        return ,$output
    }
    finally {
        $ErrorActionPreference = $prev
    }
}

Write-Host "`n=== F-17: AArch64 compile gate ===" -ForegroundColor Cyan
$armCheck = Invoke-Native cargo @(
    "check",
    "-p", "graphos-kernel",
    "--target", "aarch64-unknown-none",
    "-Z", "build-std=core,alloc,compiler_builtins",
    "--features", "freestanding"
)
if ($LASTEXITCODE -eq 0) { Pass "F17 cargo check aarch64" }
else {
    Fail "F17 cargo check aarch64" "exit $LASTEXITCODE"
    $armCheck | Select-String "^error" | Select-Object -First 20 | ForEach-Object { Write-Host $_ }
}

if (-not $SkipArmBoot) {
    Write-Host "`n=== F-17: AArch64 QEMU boot marker ===" -ForegroundColor Cyan
    $armBoot = Invoke-Native powershell.exe @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", "scripts\boot-arm64.ps1",
        "-TimeoutSec", [string]$Arm64TimeoutSec
    )
    if ($LASTEXITCODE -eq 0) { Pass "F17 arm64 boot marker" }
    else {
        Fail "F17 arm64 boot marker" "exit $LASTEXITCODE"
        $armBoot | Select-Object -Last 25 | ForEach-Object { Write-Host $_ }
    }
}

Write-Host "`n=== F-18: x86_64 compile gate ===" -ForegroundColor Cyan
$xCheck = Invoke-Native cargo @(
    "check",
    "-p", "graphos-kernel",
    "--target", "x86_64-unknown-none",
    "-Z", "build-std=core,alloc,compiler_builtins",
    "--features", "freestanding"
)
if ($LASTEXITCODE -eq 0) { Pass "F18 cargo check x86_64" }
else {
    Fail "F18 cargo check x86_64" "exit $LASTEXITCODE"
    $xCheck | Select-String "^error" | Select-Object -First 20 | ForEach-Object { Write-Host $_ }
}

Write-Host "`n=== F-18: implementation wiring checks ===" -ForegroundColor Cyan
$nvmeWrite = Select-String -Path "kernel\src\drivers\storage\nvme.rs" -Pattern "opcode: Write|pub fn write_sectors\(" -Quiet
$wifiProbe = Select-String -Path "kernel\src\drivers\net\wifi\mod.rs" -Pattern "pub fn probe_driver\(" -Quiet
$btProbe = Select-String -Path "kernel\src\drivers\bt\mod.rs" -Pattern "pub fn probe_driver\(" -Quiet
$builtinWifi = Select-String -Path "kernel\src\drivers\mod.rs" -Pattern 'name:\s*b"wifi0"' -Quiet
$builtinBt = Select-String -Path "kernel\src\drivers\mod.rs" -Pattern 'name:\s*b"bluetooth0"' -Quiet

if ($nvmeWrite) { Pass "F18 NVMe write path present" }
else { Fail "F18 NVMe write path" "write path not detected" }

if ($wifiProbe -and $builtinWifi) { Pass "F18 Wi-Fi probe integrated" }
else { Fail "F18 Wi-Fi probe integrated" "probe hook or builtin registration missing" }

if ($btProbe -and $builtinBt) { Pass "F18 Bluetooth probe integrated" }
else { Fail "F18 Bluetooth probe integrated" "probe hook or builtin registration missing" }

Write-Host ""
if ($Failures.Count -eq 0) {
    Write-Host "=== F-17/F-18 GATES PASSED ===" -ForegroundColor Green
    exit 0
}

Write-Host "=== $($Failures.Count) F-17/F-18 GATE(S) FAILED ===" -ForegroundColor Red
$Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
exit 1
