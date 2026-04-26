#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

<#
.SYNOPSIS
GraphOS deterministic network validation harness.

.DESCRIPTION
Runs end-to-end validation using real execution evidence from
scripts/verify_full_system.ps1 and serial logs. No synthetic PASS output.

.PARAMETER Action
Validation scope: boot, dhcp, tcp, ssh, persistence, graph, all.

.PARAMETER Timeout
Boot timeout seconds passed through to verify_full_system.

.PARAMETER NoBuild
Skip rebuild when invoking verify_full_system.

.PARAMETER AllowBannerOnly
Allow SSH banner-only mode in verify_full_system (no plink automation).

.PARAMETER SkipFsck
Skip fsck sub-gate in verify_full_system.

.PARAMETER KeepArtifacts
Preserve verify_full_system ESP artifacts.

.PARAMETER Verbose
Print captured output lines from invoked scripts.
#>

param(
    [ValidateSet('boot', 'dhcp', 'tcp', 'ssh', 'persistence', 'graph', 'all')]
    [string]$Action = 'all',
    [int]$Timeout = 60,
    [switch]$NoBuild,
    [switch]$AllowBannerOnly,
    [switch]$SkipFsck,
    [switch]$KeepArtifacts,
    [switch]$Verbose
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$Failures = [System.Collections.Generic.List[string]]::new()

function Pass([string]$label) {
    Write-Host "[PASS] $label" -ForegroundColor Green
}

function Fail([string]$label, [string]$detail) {
    Write-Host "[FAIL] $label : $detail" -ForegroundColor Red
    $Failures.Add("$label : $detail")
}

function Read-TextOrEmpty([string]$path) {
    if (-not (Test-Path $path)) {
        return ""
    }
    return Get-Content $path -Raw -ErrorAction SilentlyContinue
}

function Ready-Count([string]$text) {
    $healthReady = ([regex]::Matches($text, "health=ready")).Count
    if ($healthReady -gt 0) {
        return $healthReady
    }

    $serviceReady = ([regex]::Matches($text, "protected bootstrap event: service-ready:")).Count
    $uinitOnline = if ($text -match "protected bootstrap event: uinit-online") { 1 } else { 0 }
    $servicemgrOnline = if ($text -match "protected bootstrap event: servicemgr-online") { 1 } else { 0 }
    return $serviceReady + $uinitOnline + $servicemgrOnline
}

function Run-VerifyFullSystem {
    $args = @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", "scripts/verify_full_system.ps1",
        "-BootTimeoutSec", "$Timeout"
    )

    if ($NoBuild) { $args += "-NoBuild" }
    if ($AllowBannerOnly) { $args += "-AllowBannerOnly" }
    if ($SkipFsck) { $args += "-SkipFsck" }
    if ($KeepArtifacts) { $args += "-KeepArtifacts" }

    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & powershell.exe @args 2>&1
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $prev
    }

    if ($Verbose) {
        $output | ForEach-Object { Write-Host $_ }
    }

    return [PSCustomObject]@{
        ExitCode = $exitCode
        Output = ($output -join "`n")
    }
}

function Validate-VirtioNetWiring {
    $mainRsPath = "kernel/src/main.rs"
    if (-not (Test-Path $mainRsPath)) {
        Fail "virtio-net wiring" "missing $mainRsPath"
        return
    }

    $content = Get-Content $mainRsPath -Raw
    if ($content -match "pic::unmask\(net_irq\)") {
        Pass "virtio-net IRQ unmask wiring present"
    } else {
        Fail "virtio-net IRQ unmask wiring" "pic::unmask(net_irq) not found"
    }
}

Write-Host "GraphOS deterministic network validation ($Action)" -ForegroundColor Cyan

$result = Run-VerifyFullSystem
if ($result.ExitCode -ne 0) {
    Fail "verify_full_system" "exit $($result.ExitCode)"
    $tail = ($result.Output -split "`n") | Select-Object -Last 25
    $tail | ForEach-Object { Write-Host $_ }
    Write-Host ""
    Write-Host "=== NETWORK VALIDATE: FAIL ($($Failures.Count)) ===" -ForegroundColor Red
    exit 1
}
Pass "verify_full_system"

Validate-VirtioNetWiring

$boot1LogPath = "target/verify-full-system-boot1.serial.txt"
$boot2LogPath = "target/verify-full-system-boot2.serial.txt"
$boot1 = Read-TextOrEmpty $boot1LogPath
$boot2 = Read-TextOrEmpty $boot2LogPath

if (-not $boot1) {
    Fail "boot1 serial" "missing or empty $boot1LogPath"
}
if (-not $boot2) {
    Fail "boot2 serial" "missing or empty $boot2LogPath"
}

$wantBoot = $Action -in @('boot', 'all')
$wantDhcp = $Action -in @('dhcp', 'all')
$wantTcp = $Action -in @('tcp', 'all')
$wantSsh = $Action -in @('ssh', 'all')
$wantPersistence = $Action -in @('persistence', 'all')
$wantGraph = $Action -in @('graph', 'all')

if ($wantBoot) {
    $boot1Ready = Ready-Count $boot1
    $boot2Ready = Ready-Count $boot2
    if ($boot1Ready -ge 8 -and $boot2Ready -ge 8) {
        Pass "boot readiness evidence (boot1=$boot1Ready, boot2=$boot2Ready)"
    } else {
        Fail "boot readiness evidence" "expected >=8 ready markers per boot, got boot1=$boot1Ready boot2=$boot2Ready"
    }
}

if ($wantDhcp) {
    $dhcpPattern = "dhcp|10\.0\.2\.|lease|gateway|arp"
    if ($boot1 -match $dhcpPattern -or $boot2 -match $dhcpPattern) {
        Pass "DHCP/network address evidence present in serial log"
    } elseif ($result.Output -match "SSH banner reachable") {
        Pass "DHCP/network evidence inferred from successful SSH banner reachability"
    } else {
        Fail "DHCP/network evidence" "no DHCP/network markers in logs and no SSH banner evidence"
    }
}

if ($wantTcp) {
    $hasListen = ($result.Output -match "Boot1 sshd listening") -and ($result.Output -match "Boot2 sshd listening")
    $hasBanner = $result.Output -match "SSH banner reachable"
    if ($hasListen -and $hasBanner) {
        Pass "TCP handshake evidence via sshd listen + SSH banner"
    } else {
        Fail "TCP handshake evidence" "missing sshd listen or SSH banner confirmation"
    }
}

if ($wantSsh) {
    if ($AllowBannerOnly) {
        if ($result.Output -match "SSH banner reachable") {
            Pass "SSH banner-only mode"
        } else {
            Fail "SSH banner-only mode" "SSH banner evidence not found"
        }
    } else {
        if ($result.Output -match "SSH login and command execution") {
            Pass "SSH login and command execution"
        } else {
            Fail "SSH login and command execution" "plink automation evidence not found"
        }
    }
}

if ($wantPersistence) {
    $writeOk = $result.Output -match "Write persistence marker"
    $readOk = $result.Output -match "Read persistence marker after reboot"
    if ($writeOk -and $readOk) {
        Pass "persistence marker write/read across reboot"
    } else {
        Fail "persistence marker" "write/read persistence evidence missing"
    }
}

if ($wantGraph) {
    $log = "$boot1`n$boot2"
    $graphLines = ([regex]::Matches($log, "\[graph\]")).Count
    $taskNodes = ([regex]::Matches($log, "task node=")).Count
    $serviceEvents = ([regex]::Matches($log, "service-ready:")).Count
    if ($graphLines -gt 0 -and $taskNodes -gt 0 -and $serviceEvents -gt 0) {
        Pass "graph tracking evidence (graph=$graphLines taskNodes=$taskNodes serviceEvents=$serviceEvents)"
    } else {
        Fail "graph tracking evidence" "expected graph/task/service evidence not found"
    }
}

Write-Host ""
if ($Failures.Count -eq 0) {
    Write-Host "=== NETWORK VALIDATE: PASS ===" -ForegroundColor Green
    exit 0
}

Write-Host "=== NETWORK VALIDATE: FAIL ($($Failures.Count)) ===" -ForegroundColor Red
$Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
exit 1
