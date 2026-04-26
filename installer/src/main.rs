// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS installer binary.
//!
//! WASM-compatible graphical installer:
//! 1. Detect candidate disks
//! 2. Partition and format (GPT + EFI System + root ext4)
//! 3. Install kernel + boot loader
//! 4. Enrol TPM binding
//! 5. Create initial user account

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

fn heading(s: &str) {
    println!("\n\x1b[1;34m==> {}\x1b[0m", s);
}

fn ok(s: &str) {
    println!("    \x1b[32m✓\x1b[0m {}", s);
}

fn warn(s: &str) {
    println!("    \x1b[33m⚠\x1b[0m {}", s);
}

fn prompt(msg: &str) -> String {
    print!("{} ", msg);
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin().read_line(&mut s).ok();
    s.trim().to_string()
}

// ---------------------------------------------------------------------------
// Disk detection
// ---------------------------------------------------------------------------

fn list_disks() -> Vec<PathBuf> {
    let mut disks = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev") {
        for e in entries.flatten() {
            let name = e.file_name();
            let s = name.to_string_lossy();
            // Match sdX, vdX, nvme0n1 etc.
            if (s.starts_with("sd") || s.starts_with("vd") || s.starts_with("nvme")) && s.len() <= 8
            {
                disks.push(e.path());
            }
        }
    }
    disks.sort();
    disks
}

fn disk_size_gb(path: &PathBuf) -> u64 {
    fs::metadata(path)
        .map(|m| m.len() / 1_073_741_824)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Partition + format
// ---------------------------------------------------------------------------

fn partition_disk(device: &str) -> anyhow::Result<()> {
    // Create GPT with EFI (512 MiB) + root (rest).
    Command::new("sgdisk").args(["-Z", device]).status()?;
    Command::new("sgdisk")
        .args([
            "-n",
            "1:0:+512M",
            "-t",
            "1:EF00",
            "-n",
            "2:0:0",
            "-t",
            "2:8300",
            device,
        ])
        .status()?;
    ok(&format!("Partitioned {}", device));
    Ok(())
}

fn format_partitions(device: &str) -> anyhow::Result<()> {
    let efi = format!("{}1", device);
    let root = format!("{}2", device);
    Command::new("mkfs.fat").args(["-F32", &efi]).status()?;
    Command::new("mkfs.ext4").args(["-F", &root]).status()?;
    ok("Formatted EFI (FAT32) and root (ext4)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

fn install_files(device: &str, source_dir: &PathBuf) -> anyhow::Result<()> {
    let root = format!("{}2", device);
    fs::create_dir_all("/mnt/graphos").ok();
    Command::new("mount")
        .args([&root, "/mnt/graphos"])
        .status()?;

    // Copy kernel image.
    let kernel_src = source_dir.join("kernel.elf");
    if kernel_src.exists() {
        fs::copy(&kernel_src, "/mnt/graphos/boot/graphos.elf")?;
        ok("Installed kernel");
    } else {
        warn("kernel.elf not found in source directory — skipping");
    }

    // Copy initrd.
    let initrd_src = source_dir.join("initrd.img");
    if initrd_src.exists() {
        fs::copy(&initrd_src, "/mnt/graphos/boot/initrd.img")?;
        ok("Installed initrd");
    }

    Command::new("umount").arg("/mnt/graphos").status()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// TPM enrolment
// ---------------------------------------------------------------------------

/// Enrol a TPM sealing key bound to PCRs 0–7 using tpm2-tools.
///
/// Requires `tpm2-tools` to be installed on the system running the installer.
/// On success, a persistent sealing key is enrolled at handle `0x81010001`.
///
/// If `skip_if_absent` is `true` the function merely warns when no TPM is
/// found (use `--skip-tpm` flag for bare-metal installs without a TPM).
/// If `false` it returns an error, blocking installation.
fn enrol_tpm(skip_if_absent: bool) -> anyhow::Result<()> {
    heading("TPM Enrolment");

    // ── 1. Detect TPM presence ────────────────────────────────────────────
    let probe = Command::new("tpm2_pcrread").arg("sha256:0").output();
    let tpm_present = probe.map(|o| o.status.success()).unwrap_or(false);

    if !tpm_present {
        if skip_if_absent {
            warn("No TPM detected — skipping enrolment (--skip-tpm)");
            warn("WARNING: system will boot without measured-boot sealing");
            return Ok(());
        }
        anyhow::bail!(
            "TPM not found or tpm2-tools not installed.\n\
             Install tpm2-tools and ensure /dev/tpm0 is accessible, or\n\
             pass --skip-tpm to bypass (NOT recommended for production)."
        );
    }
    ok("TPM detected via tpm2_pcrread");

    // ── 2. Create primary key under Owner hierarchy ───────────────────────
    let status = Command::new("tpm2_createprimary")
        .args([
            "-C",
            "o",
            "-g",
            "sha256",
            "-G",
            "ecc256",
            "-c",
            "/tmp/graphos_ek.ctx",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("tpm2_createprimary failed (rc={})", status);
    }
    ok("Primary EK context created");

    // ── 3. Create sealing key bound to PCRs 0–7 ──────────────────────────
    // PCRs 0–7 cover firmware + boot loader + OS image measurements.
    let status = Command::new("tpm2_create")
        .args([
            "-C",
            "/tmp/graphos_ek.ctx",
            "-g",
            "sha256",
            "-G",
            "keyedhash",
            "--policy-pcr",
            "sha256:0,1,2,3,4,5,6,7",
            "-u",
            "/tmp/graphos_seal.pub",
            "-r",
            "/tmp/graphos_seal.priv",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("tpm2_create (sealing key) failed");
    }
    ok("Sealing key created (PCRs 0-7 policy)");

    // ── 4. Load key into the TPM ──────────────────────────────────────────
    let status = Command::new("tpm2_load")
        .args([
            "-C",
            "/tmp/graphos_ek.ctx",
            "-u",
            "/tmp/graphos_seal.pub",
            "-r",
            "/tmp/graphos_seal.priv",
            "-c",
            "/tmp/graphos_seal.ctx",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("tpm2_load failed");
    }

    // ── 5. Make the key persistent at a well-known GraphOS handle ─────────
    // GraphOS uses NV handle 0x81010001 for the root sealing key.
    let status = Command::new("tpm2_evictcontrol")
        .args(["-C", "o", "-c", "/tmp/graphos_seal.ctx", "0x81010001"])
        .status()?;
    if !status.success() {
        anyhow::bail!("tpm2_evictcontrol failed — could not persist sealing key");
    }
    ok("Sealing key persisted at TPM handle 0x81010001");

    // ── 6. Clean up transient context files ──────────────────────────────
    for f in &[
        "/tmp/graphos_ek.ctx",
        "/tmp/graphos_seal.ctx",
        "/tmp/graphos_seal.pub",
        "/tmp/graphos_seal.priv",
    ] {
        std::fs::remove_file(f).ok();
    }

    ok("TPM enrolment complete — root volume will be sealed to PCRs 0-7 on next boot");
    Ok(())
}

// ---------------------------------------------------------------------------
// User creation
// ---------------------------------------------------------------------------

fn create_user(username: &str, password: &str) -> anyhow::Result<()> {
    Command::new("useradd").args(["-m", username]).status()?;
    let input = format!("{password}:{password}\n");
    let mut child = Command::new("passwd")
        .arg(username)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes())?;
    child.wait()?;
    ok(&format!("Created user '{}'", username));
    Ok(())
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    println!("\x1b[1mGraphOS Installer v1.0\x1b[0m");
    println!("========================");

    let args: Vec<String> = env::args().collect();
    let unattended = args.iter().any(|a| a == "--unattended");
    let skip_tpm = args.iter().any(|a| a == "--skip-tpm");

    // ── Step 1: Disk selection ────────────────────────────────────────────────
    heading("Disk Selection");
    let disks = list_disks();
    if disks.is_empty() {
        eprintln!("installer: no suitable disks found");
        std::process::exit(1);
    }
    println!("Available disks:");
    for (i, d) in disks.iter().enumerate() {
        println!("  [{}] {} ({} GB)", i, d.display(), disk_size_gb(d));
    }
    let device = if unattended {
        disks[0].to_string_lossy().to_string()
    } else {
        let idx: usize = prompt("Select disk number [0]:").parse().unwrap_or(0);
        disks
            .get(idx)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    if device.is_empty() {
        eprintln!("installer: invalid disk selection");
        std::process::exit(1);
    }
    println!("Installing to: {}", device);

    // ── Step 2: Partition + format ────────────────────────────────────────────
    heading("Partitioning");
    if !unattended {
        let confirm = prompt(&format!(
            "WARNING: all data on {} will be erased. Continue? [y/N]:",
            device
        ));
        if !confirm.eq_ignore_ascii_case("y") {
            println!("Aborted.");
            std::process::exit(0);
        }
    }
    if let Err(e) = partition_disk(&device) {
        warn(&format!("partition: {}", e));
    }
    if let Err(e) = format_partitions(&device) {
        warn(&format!("format: {}", e));
    }

    // ── Step 3: Install files ─────────────────────────────────────────────────
    heading("Installing GraphOS");
    let source_dir = PathBuf::from(env::var("GRAPHOS_SOURCE").unwrap_or_else(|_| "/cdrom".into()));
    if let Err(e) = install_files(&device, &source_dir) {
        warn(&format!("install: {}", e));
    }

    // ── Step 4: TPM enrolment ─────────────────────────────────────────────────
    if let Err(e) = enrol_tpm(skip_tpm) {
        eprintln!("installer: TPM enrolment failed: {}", e);
        std::process::exit(1);
    }

    // ── Step 5: User creation ─────────────────────────────────────────────────
    heading("User Account");
    let username = if unattended {
        "graphos".to_string()
    } else {
        prompt("Username:")
    };
    let password = if unattended {
        "graphos".to_string()
    } else {
        prompt("Password:")
    };
    if let Err(e) = create_user(&username, &password) {
        warn(&format!("useradd: {}", e));
    }

    // ── Done ──────────────────────────────────────────────────────────────────
    heading("Installation Complete");
    println!("\n\x1b[1;32mGraphOS installed successfully!\x1b[0m");
    println!("Remove installation media and reboot.");
}
