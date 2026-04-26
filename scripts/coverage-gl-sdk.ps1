# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
param(
    [switch]$OpenReport,
    [switch]$Json
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$tool = cargo llvm-cov --version 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "cargo-llvm-cov not found. Install with: cargo install cargo-llvm-cov" -ForegroundColor Yellow
    exit 2
}

$args = @("llvm-cov", "--manifest-path", "sdk/gl-sdk/Cargo.toml", "--summary-only")
if ($Json) {
    $args += "--json"
}
if ($OpenReport) {
    $args += @("--html", "--open")
}

& cargo @args
exit $LASTEXITCODE
