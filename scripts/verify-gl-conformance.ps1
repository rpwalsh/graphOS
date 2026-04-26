#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS GL Conformance Triage Script
#
# Purpose: Run the graphos-gl test suite and map results to Khronos ES 3.0
#          conformance categories for triage/reporting.
#
# Usage:
#   .\scripts\verify-gl-conformance.ps1
#   .\scripts\verify-gl-conformance.ps1 -Filter "format_matrix"
#   .\scripts\verify-gl-conformance.ps1 -Verbose
#
# Exit codes:
#   0 = all conformance gates passed
#   1 = one or more gates failed
#
# Conformance categories (mirroring Khronos dEQP / CTS test module names):
#   ES3-cts.texture           -> texture upload, format/type matrix, mipmap
#   ES3-cts.buffers           -> VBO, UBO, SSBO, atomic, TFO
#   ES3-cts.shaders           -> GLSL link/validate semantics
#   ES3-cts.framebuffer       -> FBO, MRT, read/draw buffers, blit
#   ES3-cts.rasterization     -> pipeline, depth, stencil, blend
#   ES3-cts.state_management  -> get/set parameter parity
#   ES3-cts.diagnostics       -> debug/robustness/query objects
#   ES3-cts.extensions        -> extension reporting + gates

param(
    [string]$Filter = "",
    [switch]$Verbose,
    [switch]$Json
)

$ErrorActionPreference = "SilentlyContinue"

$WORKSPACE = $PSScriptRoot | Split-Path -Parent | Split-Path -Parent
Set-Location $WORKSPACE

# -- Category to test-name-pattern mapping --
$ConformanceMap = [ordered]@{
    "ES3-cts.texture"          = @(
        "tex_image", "tex_storage", "tex_sub", "texture_view", "tex_parameter",
        "sampler", "format_matrix", "mipmap"
    )
    "ES3-cts.buffers"          = @(
        "vbo", "buffer", "ubo", "ssbo", "atomic", "tfb", "tf_", "map_buffer",
        "transform_feedback"
    )
    "ES3-cts.shaders"          = @(
        "glsl", "shader", "program", "link", "uniform", "attrib", "varying"
    )
    "ES3-cts.framebuffer"      = @(
        "fbo", "framebuffer", "renderbuffer", "blit", "draw_buffers",
        "read_buffer", "mrt", "attachment"
    )
    "ES3-cts.rasterization"    = @(
        "draw_", "raster", "depth", "stencil", "blend", "cull", "viewport",
        "scissor", "pipeline"
    )
    "ES3-cts.state_management" = @(
        "pixel_store", "color_mask", "active_texture", "bind_", "gen_", "delete_"
    )
    "ES3-cts.diagnostics"      = @(
        "debug", "error", "robustness", "query", "sync", "fence"
    )
    "ES3-cts.extensions"       = @(
        "extension"
    )
}

# -- Run cargo test and capture output --
Write-Host "Running graphos-gl test suite..." -ForegroundColor Cyan

$ManifestPath = Join-Path $WORKSPACE "graphos\sdk\gl-sdk\Cargo.toml"
$TempOut = [System.IO.Path]::GetTempFileName()

if ($Filter -ne "") {
    & cargo test --manifest-path $ManifestPath $Filter -- --nocapture *>&1 | Out-File -FilePath $TempOut -Encoding utf8
} else {
    & cargo test --manifest-path $ManifestPath -- --nocapture *>&1 | Out-File -FilePath $TempOut -Encoding utf8
}

$raw = Get-Content $TempOut -Encoding utf8
Remove-Item $TempOut -ErrorAction SilentlyContinue
$exitCode = $LASTEXITCODE

# Parse test lines: "test <name> ... ok" or "test <name> ... FAILED"
$allTests = [System.Collections.Generic.List[PSCustomObject]]::new()
foreach ($line in $raw) {
    if ($line -match "^test\s+(\S+)\s+\.\.\.\s+(ok|FAILED|ignored)") {
        $allTests.Add([PSCustomObject]@{
            Name   = $Matches[1]
            Result = $Matches[2]
        })
    }
}

$passed = ($allTests | Where-Object Result -eq "ok").Count
$failed = ($allTests | Where-Object Result -eq "FAILED").Count
$ignored = ($allTests | Where-Object Result -eq "ignored").Count

Write-Host ""
Write-Host "Total: $($allTests.Count) tests - $passed passed, $failed failed, $ignored ignored" -ForegroundColor $(if ($failed -gt 0) { "Red" } else { "Green" })
Write-Host ""

# -- Triage into conformance categories --
$categoryResults = [ordered]@{}
foreach ($cat in $ConformanceMap.Keys) {
    $categoryResults[$cat] = [System.Collections.Generic.List[PSCustomObject]]::new()
}
$uncategorized = [System.Collections.Generic.List[PSCustomObject]]::new()

foreach ($test in $allTests) {
    $assigned = $false
    foreach ($cat in $ConformanceMap.Keys) {
        foreach ($pat in $ConformanceMap[$cat]) {
            if ($test.Name -like "*$pat*") {
                $categoryResults[$cat].Add($test)
                $assigned = $true
                break
            }
        }
        if ($assigned) { break }
    }
    if (-not $assigned) {
        $uncategorized.Add($test)
    }
}

# -- Print triage report --
$allPassed = $true
foreach ($cat in $categoryResults.Keys) {
    $tests = $categoryResults[$cat]
    $catPassed = @($tests | Where-Object Result -eq "ok").Count
    $catFailed = @($tests | Where-Object Result -eq "FAILED").Count
    $status = if ($catFailed -gt 0) { "FAIL" } else { "PASS" }
    $color = if ($catFailed -gt 0) { "Red" } else { "Green" }
    Write-Host ("  [{0,-4}] {1,-35} {2,3} pass  {3,2} fail" -f $status, $cat, $catPassed, $catFailed) -ForegroundColor $color
    if ($catFailed -gt 0) { $allPassed = $false }
    if ($Verbose) {
        foreach ($t in $tests) {
            $tc = if ($t.Result -eq "ok") { "DarkGray" } else { "Red" }
            Write-Host ("          {0,-60} {1}" -f $t.Name, $t.Result) -ForegroundColor $tc
        }
    }
}

if ($uncategorized.Count -gt 0) {
    $ucPassed = ($uncategorized | Where-Object Result -eq "ok").Count
    $ucFailed = ($uncategorized | Where-Object Result -eq "FAILED").Count
    Write-Host ("  [INFO] {0,-35} {1,3} pass  {2,2} uncategorized" -f "(other)", $ucPassed, $ucFailed)
}

Write-Host ""

# -- JSON output for CI integration --
if ($Json) {
    $report = [ordered]@{
        timestamp  = (Get-Date -Format "o")
        total      = $allTests.Count
        passed     = $passed
        failed     = $failed
        categories = @{}
    }
    foreach ($cat in $categoryResults.Keys) {
        $tests = $categoryResults[$cat]
        $report.categories[$cat] = [ordered]@{
            passed = ($tests | Where-Object Result -eq "ok").Count
            failed = ($tests | Where-Object Result -eq "FAILED").Count
            tests  = @($tests | ForEach-Object { [ordered]@{ name = $_.Name; result = $_.Result } })
        }
    }
    $report | ConvertTo-Json -Depth 6
}

# -- Final gate --
if ($failed -gt 0 -or -not $allPassed) {
    Write-Host "CONFORMANCE GATE: FAILED ($failed test(s) failing)" -ForegroundColor Red
    exit 1
} else {
    Write-Host "CONFORMANCE GATE: PASSED ($passed tests across all categories)" -ForegroundColor Green
    exit 0
}
