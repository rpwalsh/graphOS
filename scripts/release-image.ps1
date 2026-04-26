#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS release artifact builder.
#
# Stages the kernel, UEFI loader, host tools, release notes, and checksum
# manifest into a release directory and packs it as a zip archive.

param(
    [string]$OutDir = "target\release-image",
    [switch]$SkipVerify,
    [switch]$SkipBoot
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

function Require-Path([string]$Path, [string]$Label) {
    if (-not (Test-Path $Path)) {
        throw "$Label missing: $Path"
    }
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @()
    )

    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $FilePath @Arguments 2>&1
        $output | ForEach-Object { Write-Host $_ }
        if ($LASTEXITCODE -ne 0) {
            throw "$FilePath exited with code $LASTEXITCODE"
        }
    }
    finally {
        $ErrorActionPreference = $previous
    }
}

if (-not $SkipVerify) {
    $verifyArgs = @("-File", (Join-Path $PSScriptRoot "verify.ps1"), "-Release")
    if ($SkipBoot) {
        $verifyArgs += "-SkipBoot"
    }
    & powershell @verifyArgs
    if ($LASTEXITCODE -ne 0) {
        throw "verify.ps1 -Release failed"
    }
}

$releaseRoot = Join-Path $RepoRoot $OutDir
$espDir = Join-Path $releaseRoot "esp"
$toolDir = Join-Path $releaseRoot "tools"
$docsDir = Join-Path $releaseRoot "docs\release\1.0"

Remove-Item $releaseRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $espDir "EFI\BOOT") | Out-Null
New-Item -ItemType Directory -Force -Path $toolDir | Out-Null
New-Item -ItemType Directory -Force -Path $docsDir | Out-Null

Write-Host "Building release artifacts..." -ForegroundColor Cyan
Invoke-Native cargo @("build", "-p", "graphos-kernel", "--release", "--target", "x86_64-unknown-none", "-Z", "build-std=core,alloc,compiler_builtins", "--features", "freestanding")

Invoke-Native cargo @("build", "-p", "graphos-uefi-loader", "--release", "--target", "x86_64-unknown-uefi", "-Z", "build-std=core,alloc,compiler_builtins", "--features", "uefi-app")

Invoke-Native cargo @("build", "-p", "gpm", "--release")

Invoke-Native cargo @("build", "-p", "graphos-appstore", "--release")

$kernelPath = "target\x86_64-unknown-none\release\graphos-kernel"
$loaderPath = "target\x86_64-unknown-uefi\release\graphos-uefi-loader.efi"
$gpmPath = "target\release\gpm.exe"
$gpsPath = "target\release\gps.exe"
$releaseNotes = "docs\release\1.0\release-notes.md"

Require-Path $kernelPath "Kernel artifact"
Require-Path $loaderPath "UEFI loader artifact"
Require-Path $gpmPath "gpm artifact"
Require-Path $gpsPath "gps artifact"
Require-Path $releaseNotes "Release notes"

Copy-Item $kernelPath (Join-Path $espDir "kernel") -Force
Copy-Item $loaderPath (Join-Path $espDir "EFI\BOOT\BOOTX64.EFI") -Force
Copy-Item $gpmPath (Join-Path $toolDir "gpm.exe") -Force
Copy-Item $gpsPath (Join-Path $toolDir "gps.exe") -Force
Copy-Item $releaseNotes (Join-Path $docsDir "release-notes.md") -Force

if (Test-Path "docs\release-key.pub") {
    Copy-Item "docs\release-key.pub" (Join-Path $releaseRoot "release-key.pub") -Force
}

$hashLines = Get-ChildItem $releaseRoot -Recurse -File |
    Where-Object { $_.Name -ne "SHA256SUMS.txt" } |
    ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $relative = $_.FullName.Substring($releaseRoot.Length).TrimStart('\\')
        "$hash  $relative"
    }

$hashLines | Set-Content (Join-Path $releaseRoot "SHA256SUMS.txt")

$archivePath = Join-Path $RepoRoot "target\graphos-release.zip"
if (Test-Path $archivePath) {
    Remove-Item $archivePath -Force
}
Compress-Archive -Path (Join-Path $releaseRoot "*") -DestinationPath $archivePath

Write-Host "Release staged at: $releaseRoot" -ForegroundColor Green
Write-Host "Archive written to: $archivePath" -ForegroundColor Green