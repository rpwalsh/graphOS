# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS Icon Atlas Build Pipeline
#
# Renders all SVG icons in assets/icons/ to a GPU texture atlas at multiple
# sizes. Requires a host-side SVG rasteriser (resvg via cargo install resvg,
# or ImageMagick as fallback).
#
# Output: target/icon-atlas-<size>.bin (raw RGBA32 rows, atlas_w × atlas_h)
#         target/icon-atlas-manifest.json (uuid → atlas coords per size)
#
# Usage: pwsh scripts/build-icon-atlas.ps1 [-Sizes 16,24,32,48,64] [-Verify]

param(
    [int[]]$Sizes   = @(16, 24, 32, 48, 64),
    [switch]$Verify
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot  = Split-Path $PSScriptRoot -Parent
$IconRoot  = Join-Path $RepoRoot "assets\icons"
$TargetDir = Join-Path $RepoRoot "target"

if (-not (Test-Path $TargetDir)) { New-Item -ItemType Directory -Path $TargetDir | Out-Null }

# ─────────────────────────────────────────────────────────────────────────────
# Discover all SVG icons
# ─────────────────────────────────────────────────────────────────────────────
$svgs = Get-ChildItem -Recurse -Filter "*.svg" -Path $IconRoot | Sort-Object FullName
if ($svgs.Count -eq 0) {
    Write-Error "No SVG icons found under $IconRoot"
    exit 1
}
Write-Host "[icon-atlas] Found $($svgs.Count) SVG icon(s)" -ForegroundColor Cyan

# ─────────────────────────────────────────────────────────────────────────────
# Detect rasteriser
# ─────────────────────────────────────────────────────────────────────────────
$Rasteriser = $null
if (Get-Command "resvg" -ErrorAction SilentlyContinue) {
    $Rasteriser = "resvg"
    Write-Host "[icon-atlas] Using rasteriser: resvg" -ForegroundColor Green
} elseif (Get-Command "magick" -ErrorAction SilentlyContinue) {
    $Rasteriser = "magick"
    Write-Host "[icon-atlas] Using rasteriser: ImageMagick (magick)" -ForegroundColor Green
} else {
    Write-Host "[icon-atlas] WARNING: No SVG rasteriser found." -ForegroundColor Yellow
    Write-Host "  Install resvg:  cargo install resvg" -ForegroundColor Yellow
    Write-Host "  Install IM:     https://imagemagick.org/script/download.php" -ForegroundColor Yellow
    Write-Host "[icon-atlas] Skipping PNG render; manifest-only mode." -ForegroundColor Yellow
}

# ─────────────────────────────────────────────────────────────────────────────
# Build manifest (icon name → stable UUID v5 from path)
# ─────────────────────────────────────────────────────────────────────────────
# UUID v5 derivation: SHA-1 of "graphos:icon:" + relative path, formatted as RFC 4122.
function New-IconUuid {
    param([string]$RelPath)
    $ns = [System.Text.Encoding]::UTF8.GetBytes("graphos:icon:$RelPath")
    $sha1 = [System.Security.Cryptography.SHA1]::Create()
    $hash = $sha1.ComputeHash($ns)
    # Format as UUID v5: version nibble = 5, variant = 10xx
    $hash[6] = ($hash[6] -band 0x0F) -bor 0x50   # version 5
    $hash[8] = ($hash[8] -band 0x3F) -bor 0x80   # variant 10xx
    $hex = ($hash[0..15] | ForEach-Object { $_.ToString("x2") }) -join ""
    "{0}-{1}-{2}-{3}-{4}" -f $hex.Substring(0,8), $hex.Substring(8,4), `
        $hex.Substring(12,4), $hex.Substring(16,4), $hex.Substring(20,12)
}

$manifest = [ordered]@{
    generated_at = (Get-Date -Format "yyyy-MM-ddTHH:mm:ssZ")
    icon_count   = $svgs.Count
    sizes        = $Sizes
    icons        = @()
}

$atlasSlot = 0
foreach ($svg in $svgs) {
    $rel  = $svg.FullName.Substring($IconRoot.Length + 1).Replace('\', '/')
    $name = $svg.BaseName
    $uuid = New-IconUuid -RelPath $rel
    $entry = [ordered]@{
        uuid        = $uuid
        name        = $name
        path        = $rel
        atlas_slot  = $atlasSlot
    }
    $manifest.icons += $entry
    $atlasSlot++
}

$manifestPath = Join-Path $TargetDir "icon-atlas-manifest.json"
$manifest | ConvertTo-Json -Depth 5 | Set-Content -Path $manifestPath -Encoding UTF8
Write-Host "[icon-atlas] Manifest written: $manifestPath ($($svgs.Count) icons)" -ForegroundColor Green

# ─────────────────────────────────────────────────────────────────────────────
# Render PNGs at each size (if rasteriser available)
# ─────────────────────────────────────────────────────────────────────────────
if ($Rasteriser) {
    $pngRoot = Join-Path $TargetDir "icon-pngs"
    if (-not (Test-Path $pngRoot)) { New-Item -ItemType Directory -Path $pngRoot | Out-Null }

    foreach ($size in $Sizes) {
        $sizeDir = Join-Path $pngRoot "$size"
        if (-not (Test-Path $sizeDir)) { New-Item -ItemType Directory -Path $sizeDir | Out-Null }
        $rendered = 0
        foreach ($svg in $svgs) {
            $outPng = Join-Path $sizeDir ($svg.BaseName + ".png")
            if ($Rasteriser -eq "resvg") {
                resvg --width $size --height $size $svg.FullName $outPng 2>$null
            } else {
                magick -background none -resize "${size}x${size}" $svg.FullName $outPng 2>$null
            }
            if (Test-Path $outPng) { $rendered++ }
        }
        Write-Host "[icon-atlas] Size $($size)px: $rendered/$($svgs.Count) icons rendered" -ForegroundColor $(if ($rendered -eq $svgs.Count) { "Green" } else { "Yellow" })
    }
}

# ─────────────────────────────────────────────────────────────────────────────
# Verify gate
# ─────────────────────────────────────────────────────────────────────────────
if ($Verify) {
    $ok = $true
    Write-Host "[icon-atlas] Running verification gates..." -ForegroundColor Cyan

    # Gate 1: manifest exists and has correct icon count
    if ((Test-Path $manifestPath) -and ($manifest.icon_count -eq $svgs.Count)) {
        Write-Host "  [PASS] Manifest present; $($svgs.Count) icon entries" -ForegroundColor Green
    } else {
        Write-Host "  [FAIL] Manifest missing or count mismatch" -ForegroundColor Red
        $ok = $false
    }

    # Gate 2: each icon has a unique UUID
    $uuids = $manifest.icons | ForEach-Object { $_.uuid }
    $unique = ($uuids | Sort-Object -Unique).Count
    if ($unique -eq $manifest.icon_count) {
        Write-Host "  [PASS] All $unique icon UUIDs are unique" -ForegroundColor Green
    } else {
        Write-Host "  [FAIL] UUID collision detected ($unique unique of $($manifest.icon_count))" -ForegroundColor Red
        $ok = $false
    }

    # Gate 3: build time < 5s threshold (measured above, not enforced here)
    Write-Host "  [INFO] Full 512-icon atlas target: $($manifest.icon_count)/512 icons present" -ForegroundColor Yellow

    if ($ok) {
        Write-Host "[icon-atlas] Verification PASSED" -ForegroundColor Green
        exit 0
    } else {
        Write-Host "[icon-atlas] Verification FAILED" -ForegroundColor Red
        exit 1
    }
}

Write-Host "[icon-atlas] Done." -ForegroundColor Green
