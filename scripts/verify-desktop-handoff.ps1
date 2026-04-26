#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS desktop handoff smoke gate.
#
# Fails unless serial log contains all required compositor markers:
#   1) compositor online
#   2) first present
#   3) frame counter progress
#   4) frame-tick delivery evidence
#   5) runtime display ownership transition

param(
    [switch]$NoBuild,
    [int]$TimeoutSec = 45,
    [string]$SerialLogPath = "",
    [switch]$KeepLog
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

function Stop-StaleQemu {
    Get-Process qemu-system-x86_64 -ErrorAction SilentlyContinue | Stop-Process -Force
}

$logPath = $SerialLogPath
if ([string]::IsNullOrWhiteSpace($logPath)) {
    $logPath = Join-Path "target" ("desktop-handoff-" + [Guid]::NewGuid().ToString("N") + ".log")
}

$logDir = Split-Path -Parent $logPath
if (-not [string]::IsNullOrWhiteSpace($logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}
if (Test-Path $logPath) {
    Remove-Item $logPath -Force -ErrorAction SilentlyContinue
}

Stop-StaleQemu

$bootScript = Join-Path $PSScriptRoot "boot-windows-qemu.ps1"
if (-not (Test-Path $bootScript)) {
    throw "boot wrapper not found: $bootScript"
}

$bootArgs = @(
    "-ExecutionPolicy", "Bypass",
    "-File", $bootScript,
    "-PipeFriendly",
    "-SerialLogPath", $logPath
)
if ($NoBuild) {
    $bootArgs += "-NoBuild"
}

Write-Host "[verify-desktop] launching boot wrapper" -ForegroundColor Cyan
Write-Host "[verify-desktop] serial log: $logPath" -ForegroundColor Cyan

$bootProc = Start-Process -FilePath "powershell.exe" -ArgumentList $bootArgs -PassThru
$deadline = (Get-Date).AddSeconds($TimeoutSec)

$foundOnline = $false
$foundFirstPresent = $false
$foundFrameCount = $false
$foundFrameTick = $false
$foundDisplayOwnership = $false
$fallbackMode = $false
$invalidOpcode = $false
$pageFault = $false

while ((Get-Date) -lt $deadline) {
    if (Test-Path $logPath) {
        $raw = Get-Content $logPath -Raw -ErrorAction SilentlyContinue
        if ($null -eq $raw) {
            $raw = ""
        }

        $foundOnline = $raw.Contains("[compositor] graph-shell compositor online")
        $foundFirstPresent = $raw.Contains("[compositor] first present ok")
        $foundFrameCount =
            [regex]::IsMatch($raw, "\[compositor\] frame_count=\d+") -or
            [regex]::IsMatch($raw, "\[compositor\] frame_count=\d+ frame_tick_count=\d+")
        $foundFrameTick =
            $raw.Contains("[compositor] first frame-tick received") -or
            [regex]::IsMatch($raw, "\[compositor\] frame_count=\d+ frame_tick_count=\d+")
        $foundDisplayOwnership =
            $raw.Contains("[desktop] display ownership transition: boot-fb -> virtio-gpu runtime")
        $fallbackMode = $raw.Contains("[desktop] compositor not declared; cube direct-present mode")
        $invalidOpcode = $raw.Contains("[exc] INVALID OPCODE")
        $pageFault = $raw.Contains("[exc] PAGE FAULT")

        if ($fallbackMode -or $invalidOpcode -or $pageFault) {
            break
        }

        if ($foundOnline -and $foundFirstPresent -and $foundFrameCount -and $foundFrameTick -and $foundDisplayOwnership) {
            break
        }
    }

    if ($bootProc.HasExited) {
        break
    }

    Start-Sleep -Milliseconds 250
}

Stop-StaleQemu
if ($bootProc -and -not $bootProc.HasExited) {
    $bootProc.Kill() 2>$null
}

if (Test-Path $logPath) {
    Write-Host "[verify-desktop] marker hits:" -ForegroundColor Cyan
    $patterns = @(
        "[desktop] compositor not declared; cube direct-present mode",
        "[compositor] graph-shell compositor online",
        "[compositor] first present ok",
        "[compositor] frame_count=",
        "[compositor] first frame-tick received",
        "[desktop] display ownership transition: boot-fb -> virtio-gpu runtime",
        "[exc] INVALID OPCODE",
        "[exc] PAGE FAULT"
    )
    Select-String -Path $logPath -SimpleMatch -Pattern $patterns | ForEach-Object {
        Write-Host ("  HIT|{0}|{1}" -f $_.LineNumber, $_.Line)
    }
}

if ($fallbackMode) {
    Write-Host "[FAIL] desktop handoff gate: fallback direct-present mode detected" -ForegroundColor Red
    exit 1
}

if ($invalidOpcode) {
    Write-Host "[FAIL] desktop handoff gate: compositor crashed with INVALID OPCODE" -ForegroundColor Red
    exit 1
}

if ($pageFault) {
    Write-Host "[FAIL] desktop handoff gate: compositor path hit PAGE FAULT" -ForegroundColor Red
    exit 1
}

if ($foundOnline -and $foundFirstPresent -and $foundFrameCount -and $foundFrameTick -and $foundDisplayOwnership) {
    Write-Host "[PASS] desktop handoff gate: compositor online + first present + frame progression + frame-tick + display ownership" -ForegroundColor Green
    if (-not $KeepLog) {
        Remove-Item $logPath -Force -ErrorAction SilentlyContinue
    }
    exit 0
}

Write-Host "[FAIL] desktop handoff gate: required compositor markers missing" -ForegroundColor Red
Write-Host ("       online={0} first_present={1} frame_count={2} frame_tick={3} display_ownership={4}" -f $foundOnline, $foundFirstPresent, $foundFrameCount, $foundFrameTick, $foundDisplayOwnership) -ForegroundColor Yellow
exit 1
