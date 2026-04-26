# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$testPath = Join-Path $repoRoot "sdk/gl-sdk/tests/gl_desktop_tests.rs"
$srcPath = Join-Path $repoRoot "sdk/gl-sdk/src/gl.rs"

$test = Get-Content $testPath -Raw
$methods = [regex]::Matches($test,'ctx\.([A-Za-z_][A-Za-z0-9_]*)\s*\(') |
    ForEach-Object { $_.Groups[1].Value } |
    Sort-Object -Unique

$src = Get-Content $srcPath -Raw
$pubFns = [regex]::Matches($src,'(?m)^\s*pub fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(') |
    ForEach-Object { $_.Groups[1].Value } |
    Sort-Object -Unique

$covered = $pubFns | Where-Object { $methods -contains $_ }
$uncovered = $pubFns | Where-Object { $methods -notcontains $_ }

Write-Host "public_fns=$($pubFns.Count)"
Write-Host "called_in_tests=$($covered.Count)"
Write-Host "uncovered_count=$($uncovered.Count)"
Write-Host ""
$uncovered
