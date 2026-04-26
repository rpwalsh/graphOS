#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# Wrapper for stable Windows QEMU-only development runs.
param(
    [switch]$NoBuild,
    [switch]$PipeFriendly,
    [string]$SerialLogPath = "",
    [int]$SshForwardPort = 2222
)

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$bootScript = Join-Path $scriptDir "boot.ps1"

& $bootScript -WindowsQemuOnly -NoGpu3D -NoBuild:$NoBuild -PipeFriendly:$PipeFriendly -SerialLogPath $SerialLogPath -SshForwardPort $SshForwardPort
exit $LASTEXITCODE
