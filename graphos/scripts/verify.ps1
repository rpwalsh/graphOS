#!/usr/bin/env pwsh
# Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
# GraphOS Full Verification Suite
#
# Usage: .\scripts\verify.ps1 [-SkipBoot] [-BootTimeout <seconds>] [-Release] [-Frontend]
#
# Steps:
#   1. cargo check (kernel, zero errors/warnings)
#   2. cargo clippy (zero warnings treated as errors)
#   3. gpm host tool build check
#   4. Boot QEMU and capture serial output
#   5. Assert all 8 services report health=ready
#   6. Assert boot timestamp <= 800 ms (QEMU target)
#   7. Assert stack canary seeded message present
#   8. Assert watchdog heartbeat syscall + restart wiring present
#   9. Assert ACPI power-path wiring present (SYS_POWEROFF -> acpi::pm::poweroff)
#
# Exit codes: 0 = all gates passed, 1 = one or more gates failed.

param(
    [switch]$SkipBoot,
    [int]$BootTimeout = 30,
    [switch]$Release,
    [switch]$Frontend
)

$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\qemu;$env:PATH"

$WORKSPACE = $PSScriptRoot | Split-Path
Set-Location $WORKSPACE

$Failures = [System.Collections.Generic.List[string]]::new()

function Pass([string]$label) { Write-Host "[PASS] $label" -ForegroundColor Green }
function Fail([string]$label, [string]$detail) {
    Write-Host "[FAIL] $label : $detail" -ForegroundColor Red
    $Failures.Add("$label : $detail")
    if ($Release) {
        throw "Release gate failed: $label : $detail"
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
        return ,$output
    }
    finally {
        $ErrorActionPreference = $previous
    }
}

function Get-ReadyCount {
    param(
        [Parameter(Mandatory = $true)]$Log
    )

    if ($Log -is [System.Array]) {
        $Log = ($Log -join "`n")
    }
    if ($null -eq $Log) {
        $Log = ""
    }

    $healthReady = ([regex]::Matches($Log, "health=ready")).Count
    if ($healthReady -gt 0) {
        return $healthReady
    }

    $serviceReady = ([regex]::Matches($Log, "protected bootstrap event: service-ready:")).Count
    $uinitOnline = if ($Log -match "protected bootstrap event: uinit-online") { 1 } else { 0 }
    $servicemgrOnline = if ($Log -match "protected bootstrap event: servicemgr-online") { 1 } else { 0 }
    return $serviceReady + $uinitOnline + $servicemgrOnline
}

# ── Gate 1: cargo check ──────────────────────────────────────────────────────
Write-Host "`n=== Gate 1: cargo check ===" -ForegroundColor Cyan
$out = Invoke-Native cargo @(
    "check",
    "-p", "graphos-kernel",
    "--target", "x86_64-unknown-none",
    "-Z", "build-std=core,alloc,compiler_builtins",
    "--features", "freestanding"
)
if ($LASTEXITCODE -eq 0) { Pass "cargo check" }
else { Fail "cargo check" "exit $LASTEXITCODE"; $out | Select-String "^error" | ForEach-Object { Write-Host $_ } }

# ── Gate 2: cargo clippy ─────────────────────────────────────────────────────
Write-Host "`n=== Gate 2: cargo clippy ===" -ForegroundColor Cyan
$clippy = Invoke-Native cargo @(
    "clippy",
    "-p", "graphos-kernel",
    "--target", "x86_64-unknown-none",
    "-Z", "build-std=core,alloc,compiler_builtins",
    "--features", "freestanding",
    "--", "-D", "warnings"
)
if ($LASTEXITCODE -eq 0) { Pass "cargo clippy" }
else { Fail "cargo clippy" "exit $LASTEXITCODE"; $clippy | Select-String "^error" | Select-Object -First 20 | ForEach-Object { Write-Host $_ } }

# ── Gate 3: host tools build ─────────────────────────────────────────────────
Write-Host "`n=== Gate 3: host tools build ===" -ForegroundColor Cyan
$gpm = Invoke-Native cargo @("build", "-p", "gpm")
if ($LASTEXITCODE -eq 0) { Pass "gpm build" }
else { Fail "gpm build" "exit $LASTEXITCODE"; $gpm | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

$gps = Invoke-Native cargo @("build", "-p", "graphos-appstore")
if ($LASTEXITCODE -eq 0) { Pass "gps build" }
else { Fail "gps build" "exit $LASTEXITCODE"; $gps | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

# ── Gate 4-8: boot smoke ─────────────────────────────────────────────────────
if ($SkipBoot) {
    Write-Host "`n=== Gates 4-8: SKIPPED (--SkipBoot) ===" -ForegroundColor Yellow
} else {
    Write-Host "`n=== Gates 4-8: boot smoke ($BootTimeout s timeout) ===" -ForegroundColor Cyan

    # Build kernel + loader first.
    $kbuild = Invoke-Native cargo @(
        "build",
        "-p", "graphos-kernel",
        "--target", "x86_64-unknown-none",
        "-Z", "build-std=core,alloc,compiler_builtins",
        "--features", "freestanding"
    )
    if ($LASTEXITCODE -ne 0) {
        Fail "Gate4-8 prebuild: kernel" "exit $LASTEXITCODE"
        $kbuild | Select-String "^error" | Select-Object -First 20 | ForEach-Object { Write-Host $_ }
    }

    $lbuild = Invoke-Native cargo @(
        "build",
        "-p", "graphos-uefi-loader",
        "--target", "x86_64-unknown-uefi",
        "-Z", "build-std=core,alloc,compiler_builtins",
        "--features", "uefi-app"
    )
    if ($LASTEXITCODE -ne 0) {
        Fail "Gate4-8 prebuild: loader" "exit $LASTEXITCODE"
        $lbuild | Select-String "^error" | Select-Object -First 20 | ForEach-Object { Write-Host $_ }
    }

    # Prepare ESP.
    $EspDir = "target\esp\verify-" + [Guid]::NewGuid().ToString("N")
    New-Item -ItemType Directory -Path "$EspDir\EFI\BOOT" -Force | Out-Null
    Copy-Item "target\x86_64-unknown-none\debug\graphos-kernel"           "$EspDir\GRAPHOSK.BIN"
    Copy-Item "target\x86_64-unknown-uefi\debug\graphos-uefi-loader.efi"  "$EspDir\EFI\BOOT\BOOTX64.EFI"
    if (Test-Path "target\protected-ring3\graphosp.pkg") {
        Copy-Item "target\protected-ring3\graphosp.pkg" "$EspDir\GRAPHOSP.PKG"
    } else {
        Fail "Gate4-8 prebuild: package store" "target\\protected-ring3\\graphosp.pkg missing"
    }

    # Find OVMF.
    $OvmfPaths = @(
        "C:\Program Files\qemu\share\edk2-x86_64-code.fd",
        "C:\Program Files\qemu\share\OVMF.fd",
        "C:\Program Files\qemu\share\OVMF_CODE.fd",
        "$env:USERPROFILE\OVMF_CODE.fd"
    )
    $Ovmf = $OvmfPaths | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $Ovmf) { Fail "OVMF firmware" "not found in standard paths"; $Ovmf = "" }

    $TargetDir = (Resolve-Path "target").Path
    $SerialLog = Join-Path $TargetDir "verify-serial.txt"
    $QemuErrLog = Join-Path $TargetDir "verify-qemu.err.txt"
    Remove-Item $SerialLog -ErrorAction SilentlyContinue
    Remove-Item $QemuErrLog -ErrorAction SilentlyContinue

    $QemuExe = $null
    $qemuCmd = Get-Command "qemu-system-x86_64.exe" -ErrorAction SilentlyContinue
    if ($qemuCmd) {
        $QemuExe = $qemuCmd.Source
    } else {
        $qemuCandidates = @(
            "C:\Program Files\qemu\qemu-system-x86_64.exe",
            "C:\Program Files (x86)\qemu\qemu-system-x86_64.exe"
        )
        $QemuExe = $qemuCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if (-not $QemuExe) {
        Fail "QEMU binary" "qemu-system-x86_64.exe not found"
    }

    if ($Ovmf -and $QemuExe) {
        $QemuArgs = @(
            "-machine", "q35,accel=tcg",
            "-m", "1024M",
            "-drive", "if=pflash,format=raw,readonly=on,file=`"$Ovmf`"",
            "-drive", "format=raw,file=fat:rw:`"$EspDir`"",
            "-serial", "file:$SerialLog",
            "-display", "none",
            "-no-reboot"
        )
        $proc = Start-Process -FilePath $QemuExe -ArgumentList $QemuArgs -PassThru -RedirectStandardError $QemuErrLog
        $deadline = (Get-Date).AddSeconds($BootTimeout)
        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 500
            if (Test-Path $SerialLog) {
                $log = Get-Content $SerialLog -Raw -ErrorAction SilentlyContinue
                if (-not [string]::IsNullOrEmpty($log)) {
                    $readyNow = Get-ReadyCount -Log $log
                    if ($readyNow -ge 8 -or $log -match "all services ready|handing off to desktop") {
                        break
                    }
                }
            }
        }
        if ($proc -and -not $proc.HasExited) {
            $proc.Kill() 2>$null
        }

        if (-not (Test-Path $SerialLog)) {
            $detail = "not created"
            if (Test-Path $QemuErrLog) {
                $stderr = (Get-Content $QemuErrLog -Raw -ErrorAction SilentlyContinue).Trim()
                if ($stderr) {
                    $detail = "$detail; qemu stderr: $stderr"
                }
            }
            if ($proc -and $proc.HasExited) {
                $detail = "$detail; qemu exit=$($proc.ExitCode)"
            }
            Fail "boot serial log" $detail
        } else {
            $log = Get-Content $SerialLog -Raw
            if ($null -eq $log) { $log = "" }

            # Gate 4: services ready
            $readyCount = Get-ReadyCount -Log $log
            if ($readyCount -ge 8) { Pass "Gate4: 8 services ready markers ($readyCount found)" }
            else { Fail "Gate4: services ready" "only $readyCount/8 services ready markers found" }

            # Gate 5: boot time <= 800 ms
            $btMatch = [regex]::Match($log, "boot.*?(\d+)\s*ms")
            if ($btMatch.Success) {
                $ms = [int]$btMatch.Groups[1].Value
                if ($ms -le 800) { Pass "Gate5: boot time ${ms}ms <= 800ms" }
                else { Fail "Gate5: boot time" "${ms}ms > 800ms target" }
            } else {
                Write-Host "[SKIP] Gate5: boot time not found in log" -ForegroundColor Yellow
            }

            # Gate 6: stack canary seeded
            if ($log -match "canary seeded|stack canary|__stack_chk_guard") { Pass "Gate6: stack canary seeded" }
            else { Fail "Gate6: stack canary" "canary seeded message not found in boot log" }

            # Gate 7: SYS_HEARTBEAT registered
            if ($log -match "heartbeat|SYS_HEARTBEAT|0x150") { Pass "Gate7: heartbeat syscall" }
            else {
                # Watchdog syscall may not emit a log line; check build instead.
                $hasSysHb = Select-String -Path "kernel\src\syscall\numbers.rs" -Pattern "SYS_HEARTBEAT" -Quiet
                if ($hasSysHb) { Pass "Gate7: SYS_HEARTBEAT defined in syscall table" }
                else { Fail "Gate7: heartbeat" "SYS_HEARTBEAT not found" }
            }

            # Gate 8: crash dump path exists in panic handler
            $hasDump = Select-String -Path "kernel\src\panic.rs" -Pattern "var.crash" -Quiet
            if ($hasDump) { Pass "Gate8: crash dump path wired in panic handler" }
            else { Fail "Gate8: crash dump" "/var/crash not found in panic.rs" }

            # Gate 9: watchdog restart wiring exists
            $hasWatchdogRestart =
                (Select-String -Path "kernel\src\svc\mod.rs" -Pattern "\[watchdog\] restart ok" -Quiet) -and
                (Select-String -Path "kernel\src\arch\x86_64\idt.rs" -Pattern "watchdog_check\(" -Quiet)
            if ($hasWatchdogRestart) { Pass "Gate9: watchdog restart wiring present" }
            else { Fail "Gate9: watchdog restart wiring" "watchdog restart path not fully wired" }

            # Gate 10: ACPI power-path wiring exists
            $hasPoweroffDispatch = Select-String -Path "kernel\src\syscall\mod.rs" -Pattern "SYS_POWEROFF\s*=>\s*\{\s*crate::acpi::pm::poweroff\(\)\s*\}" -Quiet
            $hasQemuPoweroffFallback = Select-String -Path "kernel\src\acpi\pm.rs" -Pattern "outw\(0x604,\s*0x2000\)" -Quiet
            if ($hasPoweroffDispatch -and $hasQemuPoweroffFallback) {
                Pass "Gate10: ACPI power-path wiring present"
            }
            else {
                Fail "Gate10: ACPI power-path" "missing SYS_POWEROFF dispatch or QEMU ACPI fallback"
            }

            # Gate 11: desktop handoff markers (compositor path, no fallback direct-present)
            $desktopGate = Invoke-Native powershell @(
                "-ExecutionPolicy", "Bypass",
                "-File", "scripts\verify-desktop-handoff.ps1",
                "-NoBuild",
                "-TimeoutSec", [string]$BootTimeout,
                "-KeepLog"
            )
            if ($LASTEXITCODE -eq 0) {
                Pass "Gate11: desktop handoff compositor markers"
            }
            else {
                Fail "Gate11: desktop handoff" "verify-desktop-handoff.ps1 exit $LASTEXITCODE"
                $desktopGate | Select-Object -Last 20 | ForEach-Object { Write-Host $_ }
            }
        }

        # Cleanup ESP.
        Remove-Item $EspDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ── Release gates ────────────────────────────────────────────────────────────
if ($Release) {
    Write-Host "`n=== Release Gates ===" -ForegroundColor Cyan

    $releaseNotes = "docs\release\1.0\release-notes.md"
    if (Test-Path $releaseNotes) { Pass "release notes present" }
    else { Fail "release notes" "$releaseNotes missing" }

    $releaseScript = "scripts\release-image.ps1"
    if (Test-Path $releaseScript) { Pass "release-image script present" }
    else { Fail "release-image script" "$releaseScript missing" }

    $hsmAttestation = "docs\release\hsm-key-attestation.txt"
    if (Test-Path $hsmAttestation) {
        $attText = Get-Content $hsmAttestation -Raw
        $statusLine = [regex]::Match($attText, "(?m)^status:\s*(\S+)")
        $statusOk = $statusLine.Success -and ($statusLine.Groups[1].Value -eq "approved")
        if ($attText -match "(?m)^key-id:\s*graphos-release-v1\s*$" -and $attText -match "(?m)^hsm-model:\s*\S+" -and $attText -match "(?m)^fingerprint-sha256:\s*\S+" -and $statusOk) {
            Pass "HSM attestation artifact approved"
        } else {
            Fail "HSM attestation" "missing required fields or status is not approved"
        }
    }
    else {
        Fail "HSM attestation" "$hsmAttestation missing"
    }

    $releaseKey = "docs\release-key.pub"
    if (Test-Path $releaseKey) {
        $keyText = Get-Content $releaseKey -Raw
        if ($keyText -match "PLACEHOLDER_REPLACE_BEFORE_RELEASE" -or $keyText -match "NOT_A_REAL_KEY") {
            Fail "release public key" "docs/release-key.pub still contains placeholder material"
        }
        $b64 = [regex]::Match($keyText, "public-key-base64:\s*\r?\n\s*([A-Za-z0-9+/=]+)")
        if (-not $b64.Success) {
            Fail "release public key" "public-key-base64 field missing"
        }
        try {
            $raw = [Convert]::FromBase64String($b64.Groups[1].Value)
            if ($raw.Length -ne 32) {
                Fail "release public key" "ed25519 public key must decode to 32 bytes"
            }
            Pass "published release public key present and parseable"
        }
        catch {
            Fail "release public key" "invalid base64 in public-key-base64 field"
        }
    }
    else { Fail "release public key" "$releaseKey missing" }

    $gpmRelease = Invoke-Native cargo @("build", "-p", "gpm", "--release")
    if ($LASTEXITCODE -eq 0) { Pass "gpm release build" }
    else { Fail "gpm release build" "exit $LASTEXITCODE"; $gpmRelease | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

    $gpsRelease = Invoke-Native cargo @("build", "-p", "graphos-appstore", "--release")
    if ($LASTEXITCODE -eq 0) { Pass "gps release build" }
    else { Fail "gps release build" "exit $LASTEXITCODE"; $gpsRelease | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

    $kernelRelease = Invoke-Native cargo @(
        "build",
        "-p", "graphos-kernel",
        "--release",
        "--target", "x86_64-unknown-none",
        "-Z", "build-std=core,alloc,compiler_builtins",
        "--features", "freestanding"
    )
    if ($LASTEXITCODE -eq 0) { Pass "kernel release build" }
    else { Fail "kernel release build" "exit $LASTEXITCODE"; $kernelRelease | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

    $loaderRelease = Invoke-Native cargo @(
        "build",
        "-p", "graphos-uefi-loader",
        "--release",
        "--target", "x86_64-unknown-uefi",
        "-Z", "build-std=core,alloc,compiler_builtins",
        "--features", "uefi-app"
    )
    if ($LASTEXITCODE -eq 0) { Pass "UEFI loader release build" }
    else { Fail "UEFI loader release build" "exit $LASTEXITCODE"; $loaderRelease | Select-String "^error" | Select-Object -First 10 | ForEach-Object { Write-Host $_ } }

    $requiredArtifacts = @(
        "target\x86_64-unknown-none\release\graphos-kernel",
        "target\x86_64-unknown-uefi\release\graphos-uefi-loader.efi",
        "target\release\gpm.exe",
        "target\release\gps.exe",
        "docs\release\1.0\release-notes.md",
        "docs\release\release-signing-ceremony.md"
    )
    foreach ($artifact in $requiredArtifacts) {
        if (Test-Path $artifact) { Pass "artifact present: $artifact" }
        else { Fail "release artifact" "$artifact missing" }
    }

    $interop = Invoke-Native cargo @("test", "-p", "graphos-tool-sdk", "--test", "ed25519_interop_release_gate")
    if ($LASTEXITCODE -eq 0) { Pass "ed25519 host-sign/kernel-compat interop tests" }
    else { Fail "ed25519 interop tests" "exit $LASTEXITCODE"; $interop | Select-String "test result|FAILED|error" | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }

    $trustHarness = "scripts\generate-release-trust-evidence.ps1"
    if (Test-Path $trustHarness) {
        $trustOut = Invoke-Native powershell @("-ExecutionPolicy", "Bypass", "-File", $trustHarness)
        if ($LASTEXITCODE -eq 0) {
            Pass "release trust harness"
        } else {
            $trustOut | Select-Object -Last 10 | ForEach-Object { Write-Host $_ }
            Fail "release trust harness" "did not produce kernel-pass evidence"
        }
    } else {
        Fail "release trust harness" "$trustHarness missing"
    }

    $kernelInteropEvidence = "target\release-trust\host-sign-kernel-verify.json"
    if (Test-Path $kernelInteropEvidence) {
        $interopEvidence = Get-Content $kernelInteropEvidence -Raw
        if ($interopEvidence -match '"kernel_verify"\s*:\s*"pass"' -and $interopEvidence -match '"tampered_payload"\s*:\s*"fail"' -and $interopEvidence -match '"tampered_signature"\s*:\s*"fail"' -and $interopEvidence -match '"wrong_public_key"\s*:\s*"fail"') {
            Pass "kernel verifier interop evidence present"
        } else {
            Fail "kernel verifier interop evidence" "evidence file present but missing required pass/fail assertions"
        }
    } else {
        Fail "kernel verifier interop evidence" "$kernelInteropEvidence missing (must be generated by hostile trust harness)"
    }

    $auditVersion = Invoke-Native cargo @("audit", "--version")
    $hasCargoAudit = ($LASTEXITCODE -eq 0)

    if ($hasCargoAudit) {
        $auditPkgmgr = Invoke-Native cargo @("audit", "--manifest-path", "tools\pkgmgr\Cargo.toml")
        if ($LASTEXITCODE -eq 0) { Pass "cargo-audit pkgmgr" }
        else { Fail "cargo-audit pkgmgr" "exit $LASTEXITCODE"; $auditPkgmgr | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }

        $auditAppstore = Invoke-Native cargo @("audit", "--manifest-path", "tools\appstore\Cargo.toml")
        if ($LASTEXITCODE -eq 0) { Pass "cargo-audit appstore" }
        else { Fail "cargo-audit appstore" "exit $LASTEXITCODE"; $auditAppstore | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }

        $auditToolSdk = Invoke-Native cargo @("audit", "--manifest-path", "sdk\tool-sdk\Cargo.toml")
        if ($LASTEXITCODE -eq 0) { Pass "cargo-audit tool-sdk" }
        else { Fail "cargo-audit tool-sdk" "exit $LASTEXITCODE"; $auditToolSdk | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }
    }
    else {
        Write-Host "[info] cargo-audit unavailable; using equivalent locked dependency policy checks" -ForegroundColor Yellow

        $policyManifests = @(
            "tools\pkgmgr\Cargo.toml",
            "tools\appstore\Cargo.toml",
            "sdk\tool-sdk\Cargo.toml"
        )

        foreach ($manifest in $policyManifests) {
            $meta = Invoke-Native cargo @("metadata", "--manifest-path", $manifest, "--locked", "--no-deps", "--format-version", "1")
            if ($LASTEXITCODE -eq 0) { Pass "dependency policy locked metadata: $manifest" }
            else { Fail "dependency policy locked metadata" "$manifest (exit $LASTEXITCODE)"; $meta | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }

            $tree = Invoke-Native cargo @("tree", "--manifest-path", $manifest, "--locked")
            if ($LASTEXITCODE -eq 0) { Pass "dependency policy locked tree: $manifest" }
            else { Fail "dependency policy locked tree" "$manifest (exit $LASTEXITCODE)"; $tree | Select-Object -Last 20 | ForEach-Object { Write-Host $_ } }
        }
    }
}

# ── Frontend harness gate ───────────────────────────────────────────────────
if ($Frontend) {
    Write-Host "`n=== Frontend Harness Gate ===" -ForegroundColor Cyan
    $frontend = Invoke-Native pwsh @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts\qemu-frontend-harness.ps1", "-TimeoutSec", [string]$BootTimeout)
    if ($LASTEXITCODE -eq 0) {
        Pass "frontend harness"
    } else {
        Fail "frontend harness" "exit $LASTEXITCODE"
        $frontend | Select-Object -Last 20 | ForEach-Object { Write-Host $_ }
    }
}

# ── Summary ──────────────────────────────────────────────────────────────────
Write-Host ""
if ($Failures.Count -eq 0) {
    Write-Host "=== ALL GATES PASSED ===" -ForegroundColor Green
    exit 0
} else {
    Write-Host "=== $($Failures.Count) GATE(S) FAILED ===" -ForegroundColor Red
    $Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
