#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS Boot Script - builds kernel + loader, creates ESP layout, boots QEMU
#
# Usage: .\scripts\boot.ps1 [-Debug] [-NoBuild]
#
# Prerequisites:
#   - Rust nightly with x86_64-unknown-none and x86_64-unknown-uefi targets
#   - QEMU with OVMF firmware
#   - Run from the graphos/ workspace root

param(
    [switch]$Debug,
    [switch]$NoBuild,
    [switch]$DisableNetwork,
    [switch]$WindowsQemuOnly,
    [switch]$Gpu3D,
    [switch]$NoGpu3D,
    [switch]$PipeFriendly,
    [string]$SerialLogPath = "",
    [int]$SshForwardPort = 2222
)

$ErrorActionPreference = "Continue"

function Test-TcpPortAvailable {
    param(
        [Parameter(Mandatory = $true)][int]$Port
    )

    $listener = $null
    try {
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
        $listener.Start()
        return $true
    }
    catch {
        return $false
    }
    finally {
        if ($listener) {
            try { $listener.Stop() } catch {}
        }
    }
}

function Resolve-SshForwardPort {
    param(
        [Parameter(Mandatory = $true)][int]$RequestedPort,
        [int]$MaxAttempts = 64
    )

    if ($RequestedPort -le 0 -or $RequestedPort -gt 65535) {
        throw "Invalid -SshForwardPort: $RequestedPort"
    }

    $port = $RequestedPort
    for ($i = 0; $i -lt $MaxAttempts -and $port -le 65535; $i++) {
        if (Test-TcpPortAvailable -Port $port) {
            return $port
        }
        $port++
    }

    throw "Unable to find free localhost SSH forward port starting at $RequestedPort"
}

# Ensure cargo and qemu are in PATH
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\qemu;$env:PATH"

$WORKSPACE = $PSScriptRoot | Split-Path
Set-Location $WORKSPACE

$KERNEL_ELF  = "target\x86_64-unknown-none\debug\graphos-kernel"
$LOADER_EFI  = "target\x86_64-unknown-uefi\debug\graphos-uefi-loader.efi"
$PACKAGE_STORE = "target\protected-ring3\graphosp.pkg"
$ESP_ROOT    = "target\esp"
$ESP_DIR     = Join-Path $ESP_ROOT ("run-" + [Guid]::NewGuid().ToString("N"))
$QEMU        = "qemu-system-x86_64.exe"

# Find OVMF firmware
$OVMF_PATHS = @(
    "C:\Program Files\qemu\share\edk2-x86_64-code.fd",
    "C:\Program Files\qemu\share\OVMF.fd",
    "C:\Program Files\qemu\share\OVMF_CODE.fd",
    "$env:USERPROFILE\OVMF_CODE.fd"
)
$OVMF = $null
foreach ($p in $OVMF_PATHS) {
    if (Test-Path $p) { $OVMF = $p; break }
}
if (-not $OVMF) {
    Write-Error "OVMF firmware not found. Searched: $($OVMF_PATHS -join ', ')"
    exit 1
}
Write-Host "[boot] OVMF: $OVMF" -ForegroundColor Cyan

# Step 1: Build kernel and loader
if (-not $NoBuild) {
    Write-Host "[boot] Building kernel..." -ForegroundColor Yellow
    if ($PipeFriendly) {
        & cargo build -p graphos-kernel --target x86_64-unknown-none "-Zbuild-std=core,compiler_builtins,alloc" --features freestanding
    } else {
        & cargo build -p graphos-kernel --target x86_64-unknown-none "-Zbuild-std=core,compiler_builtins,alloc" --features freestanding 2>&1 | Write-Host
    }
    if ($LASTEXITCODE -ne 0) { Write-Error "Kernel build failed"; exit 1 }

    Write-Host "[boot] Building loader..." -ForegroundColor Yellow
    if ($PipeFriendly) {
        & cargo build -p graphos-uefi-loader --target x86_64-unknown-uefi "-Zbuild-std=core,compiler_builtins,alloc" --features uefi-app
    } else {
        & cargo build -p graphos-uefi-loader --target x86_64-unknown-uefi "-Zbuild-std=core,compiler_builtins,alloc" --features uefi-app 2>&1 | Write-Host
    }
    if ($LASTEXITCODE -ne 0) { Write-Error "Loader build failed"; exit 1 }
}

# Verify artifacts exist
if (-not (Test-Path $KERNEL_ELF)) { Write-Error "Kernel not found: $KERNEL_ELF"; exit 1 }
if (-not (Test-Path $LOADER_EFI)) { Write-Error "Loader not found: $LOADER_EFI"; exit 1 }
if (-not (Test-Path $PACKAGE_STORE)) { Write-Error "Package store not found: $PACKAGE_STORE"; exit 1 }

$qemuCmd = Get-Command $QEMU -ErrorAction SilentlyContinue
if (-not $qemuCmd) {
    Write-Error "QEMU not found in PATH. Expected $QEMU"
    exit 1
}

$kSize = (Get-Item $KERNEL_ELF).Length
$lSize = (Get-Item $LOADER_EFI).Length
Write-Host "[boot] Kernel: $kSize bytes" -ForegroundColor Green
Write-Host "[boot] Loader: $lSize bytes" -ForegroundColor Green

# Step 2: Create ESP directory layout
#
# QEMU's -drive file=fat:rw:<dir> creates a virtual FAT filesystem from a host
# directory. This avoids needing dd/mkfs.fat/mcopy on Windows.
# Layout:
#   target/esp/EFI/BOOT/BOOTX64.EFI   <- UEFI loader
#   target/esp/GRAPHOSK.BIN            <- kernel ELF
Write-Host "[boot] Creating ESP layout..." -ForegroundColor Yellow
New-Item -ItemType Directory -Path "$ESP_DIR\EFI\BOOT" -Force | Out-Null

Copy-Item $LOADER_EFI "$ESP_DIR\EFI\BOOT\BOOTX64.EFI"
Copy-Item $KERNEL_ELF "$ESP_DIR\GRAPHOSK.BIN"
Copy-Item $PACKAGE_STORE "$ESP_DIR\GRAPHOSP.PKG"

Write-Host "[boot] ESP layout:" -ForegroundColor Cyan
Get-ChildItem -Recurse $ESP_DIR | ForEach-Object {
    $rel = $_.FullName.Substring((Resolve-Path $ESP_DIR).Path.Length)
    if ($_.PSIsContainer) {
        Write-Host "  [DIR]  $rel"
    } else {
        Write-Host "  [FILE] $rel ($($_.Length) bytes)"
    }
}

# Step 3: Launch QEMU
#
# Using -drive file=fat:rw:<dir> to present the ESP directory as a FAT volume.
# OVMF will find BOOTX64.EFI and launch it.
Write-Host ""
Write-Host "============================================" -ForegroundColor Cyan
Write-Host "  GraphOS - booting in QEMU" -ForegroundColor Cyan
Write-Host "============================================" -ForegroundColor Cyan
Write-Host "[boot] Serial output below (desktop handoff and launcher logs will appear here)" -ForegroundColor Yellow
Write-Host ""

$espFull = (Resolve-Path $ESP_DIR).Path
try {
    $isWindowsHost = $PSVersionTable.PSEdition -eq "Desktop" -or $env:OS -eq "Windows_NT"
    $windowsQemuOnlyMode = $WindowsQemuOnly -or ($isWindowsHost -and -not $Gpu3D)

    # Default to GraphOS-native submit/scanout path; virgl host GL is opt-in.
    $useGpu3D = ($Gpu3D -and (-not $NoGpu3D)) -and (-not $windowsQemuOnlyMode)

    $gpuDevice = "virtio-gpu-pci,disable-modern=on,disable-legacy=off,xres=1280,yres=800"
    $displayArg = "gtk,grab-on-hover=on,show-tabs=off,show-cursor=off"
    if ($useGpu3D) {
        # Request virgl-capable modern GPU path for true host-accelerated rendering.
        $gpuDevice = "virtio-gpu-gl-pci,xres=1280,yres=800"
        $displayArg = "gtk,gl=on,grab-on-hover=on,show-tabs=off,show-cursor=off"
        Write-Host "[boot] GPU 3D mode requested (virtio-gpu-gl + GTK GL)" -ForegroundColor Cyan
    } else {
        Write-Host "[boot] GraphOS-native GPU mode requested (virtio-gpu scanout, no host virgl)" -ForegroundColor Cyan
    }

    $cpuModel = if ($windowsQemuOnlyMode) { "qemu64" } else { "Skylake-Client-v3" }
    if ($windowsQemuOnlyMode) {
        Write-Host "[boot] Windows QEMU-only mode enabled (TCG-safe CPU profile)" -ForegroundColor Cyan
    }
    Write-Host "[boot] Input mode: PS/2 keyboard + mouse (virtio-input held back until guest support is unified)" -ForegroundColor Cyan

    $qemuArgs = @(
        "-drive", "if=pflash,format=raw,readonly=on,file=$OVMF",
        "-drive", "file=fat:rw:$espFull,format=raw",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-vga", "none",
        "-device", $gpuDevice,
        "-display", $displayArg,
        "-cpu", $cpuModel,
        "-m", "512M",
        "-monitor", "none"
    )

    if (-not [string]::IsNullOrWhiteSpace($SerialLogPath)) {
        $serialDir = Split-Path -Parent $SerialLogPath
        if (-not [string]::IsNullOrWhiteSpace($serialDir)) {
            New-Item -ItemType Directory -Path $serialDir -Force | Out-Null
        }
        if (Test-Path $SerialLogPath) {
            Remove-Item $SerialLogPath -Force -ErrorAction SilentlyContinue
        }
        $qemuArgs += @( "-serial", "file:$SerialLogPath" )
        Write-Host "[boot] Serial log file: $SerialLogPath" -ForegroundColor Cyan
    } else {
        $qemuArgs += @( "-serial", "stdio" )
    }

    if (-not $DisableNetwork) {
        $resolvedSshForwardPort = Resolve-SshForwardPort -RequestedPort $SshForwardPort
        if ($resolvedSshForwardPort -ne $SshForwardPort) {
            Write-Host "[boot] Requested SSH port $SshForwardPort is busy; using $resolvedSshForwardPort" -ForegroundColor Yellow
        }
        $qemuArgs += @(
            "-netdev", "user,id=net0,hostfwd=tcp::${resolvedSshForwardPort}-:22",
            "-device", "virtio-net-pci,disable-modern=on,netdev=net0"
        )
        Write-Host "[boot] Network enabled (SSH host forward localhost:$resolvedSshForwardPort -> guest:22)" -ForegroundColor Cyan
    }

    if ($Debug) {
        $qemuArgs += @("-d", "int,cpu_reset", "-D", "qemu-debug.log", "-s")
        Write-Host "[boot] Debug mode: GDB on :1234, log in qemu-debug.log" -ForegroundColor Magenta
    }

    $env:GDK_WIN32_DISABLE_TOUCH = "1"
    if ($PipeFriendly) {
        & $qemuCmd.Source @qemuArgs
    } else {
        & $qemuCmd.Source @qemuArgs 2>&1 | Write-Host
    }
    $qemuExit = $LASTEXITCODE
    if ($qemuExit -eq 33) {
        # isa-debug-exit reports guest-requested shutdown as (code << 1) | 1.
        exit 0
    }
    exit $qemuExit
}
finally {
    if (Test-Path $ESP_DIR) {
        Remove-Item -Recurse -Force $ESP_DIR -ErrorAction SilentlyContinue
    }
}
