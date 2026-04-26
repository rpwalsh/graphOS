#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS bare-metal USB prep helper.
#
# Builds kernel + loader, stages an ESP tree, and optionally copies it to a
# mounted FAT32 USB drive (drive root).
#
# Usage:
#   .\scripts\boot-baremetal.ps1
#   .\scripts\boot-baremetal.ps1 -UsbRoot E:\
#   .\scripts\boot-baremetal.ps1 -UsbRoot E:\ -NoBuild

param(
    [string]$UsbRoot,
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$Workspace = $PSScriptRoot | Split-Path
Set-Location $Workspace

$KernelElf = "target\x86_64-unknown-none\debug\graphos-kernel"
$LoaderEfi = "target\x86_64-unknown-uefi\debug\graphos-uefi-loader.efi"
$PackageStore = "target\protected-ring3\graphosp.pkg"
$EspRoot = "target\esp-baremetal"
$EspDir = Join-Path $EspRoot ("run-" + [Guid]::NewGuid().ToString("N"))

if (-not $NoBuild) {
    Write-Host "[baremetal] Building kernel..." -ForegroundColor Yellow
    & cargo build -p graphos-kernel --target x86_64-unknown-none "-Zbuild-std=core,compiler_builtins,alloc" --features freestanding
    if ($LASTEXITCODE -ne 0) { throw "Kernel build failed" }

    Write-Host "[baremetal] Building loader..." -ForegroundColor Yellow
    & cargo build -p graphos-uefi-loader --target x86_64-unknown-uefi "-Zbuild-std=core,compiler_builtins,alloc" --features uefi-app
    if ($LASTEXITCODE -ne 0) { throw "Loader build failed" }
}

if (-not (Test-Path $KernelElf)) { throw "Kernel not found: $KernelElf" }
if (-not (Test-Path $LoaderEfi)) { throw "Loader not found: $LoaderEfi" }
if (-not (Test-Path $PackageStore)) { throw "Package store not found: $PackageStore" }

Write-Host "[baremetal] Staging ESP layout..." -ForegroundColor Yellow
New-Item -ItemType Directory -Path "$EspDir\EFI\BOOT" -Force | Out-Null
Copy-Item $LoaderEfi "$EspDir\EFI\BOOT\BOOTX64.EFI"
Copy-Item $KernelElf "$EspDir\GRAPHOSK.BIN"
Copy-Item $PackageStore "$EspDir\GRAPHOSP.PKG"

Write-Host "[baremetal] ESP staged at: $EspDir" -ForegroundColor Green

if ($UsbRoot) {
    $usb = $UsbRoot
    if ($usb.Length -eq 2 -and $usb[1] -eq ':') {
        $usb = "$usb\\"
    }
    if (-not (Test-Path $usb)) {
        throw "USB root does not exist: $usb"
    }

    Write-Host "[baremetal] Copying staged ESP to USB root: $usb" -ForegroundColor Yellow
    New-Item -ItemType Directory -Path (Join-Path $usb "EFI\BOOT") -Force | Out-Null
    Copy-Item "$EspDir\EFI\BOOT\BOOTX64.EFI" (Join-Path $usb "EFI\BOOT\BOOTX64.EFI") -Force
    Copy-Item "$EspDir\GRAPHOSK.BIN" (Join-Path $usb "GRAPHOSK.BIN") -Force
    Copy-Item "$EspDir\GRAPHOSP.PKG" (Join-Path $usb "GRAPHOSP.PKG") -Force
    Write-Host "[baremetal] USB copy complete." -ForegroundColor Green
}

Write-Host "[baremetal] Next steps:" -ForegroundColor Cyan
Write-Host "  1) Ensure target USB is FAT32 + UEFI bootable." -ForegroundColor Cyan
Write-Host "  2) Boot target machine from USB and capture serial logs if available." -ForegroundColor Cyan
