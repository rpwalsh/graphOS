// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS OEM Image Builder
//!
//! Produces a ready-to-ship OEM disk image by:
//!   1. Starting from a release ISO (produced by `scripts/release-image.ps1`).
//!   2. Injecting an OEM branding bundle (`oem-branding.tar.gz`):
//!      - Custom boot splash (`/boot/splash.bmp`)
//!      - Custom desktop wallpaper (`/usr/share/wallpaper/oem.jpg`)
//!      - OEM-specific locale defaults (`/etc/oem/locale.conf`)
//!   3. Pre-enrolling the OEM's Secure Boot key into the EFI NVRAM image.
//!   4. Pre-installing a list of signed `.gapp` bundles into `/pkg/`.
//!   5. Writing the final bootable GPT image to `<output>.img`.
//!
//! ## Usage
//! ```
//! oem-image \
//!   --release-iso graphos-1.0.iso \
//!   --branding oem-branding.tar.gz \
//!   --sb-key oem-secureboot.cer \
//!   --preinstall app1.gapp,app2.gapp \
//!   --output my-oem-graphos.img
//! ```

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let args: Vec<String> = env::args().collect();
    let opts = parse_args(&args).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        print_usage(&args[0]);
        std::process::exit(1);
    });

    println!(
        "[oem-image] Building OEM image from: {}",
        opts.release_iso.display()
    );

    // Step 1: Verify the release ISO signature.
    verify_iso_signature(&opts.release_iso);

    // Step 2: Mount the ISO and copy to a working directory.
    let work_dir = opts.output.with_extension("workdir");
    extract_iso(&opts.release_iso, &work_dir);

    // Step 3: Inject branding.
    if let Some(ref branding) = opts.branding {
        inject_branding(branding, &work_dir);
    }

    // Step 4: Enrol the OEM Secure Boot key.
    if let Some(ref sb_key) = opts.sb_key {
        enrol_secure_boot_key(sb_key, &work_dir);
    }

    // Step 5: Pre-install signed app bundles.
    for app in &opts.preinstall {
        preinstall_app(app, &work_dir);
    }

    // Step 6: Repack to GPT image.
    repack_image(&work_dir, &opts.output);

    println!("[oem-image] Done: {}", opts.output.display());
}

// ─── Options ──────────────────────────────────────────────────────────────────

struct Opts {
    release_iso: PathBuf,
    branding: Option<PathBuf>,
    sb_key: Option<PathBuf>,
    preinstall: Vec<PathBuf>,
    output: PathBuf,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut release_iso = None;
    let mut branding = None;
    let mut sb_key = None;
    let mut preinstall = Vec::new();
    let mut output = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--release-iso" => {
                i += 1;
                release_iso = Some(PathBuf::from(&args[i]));
            }
            "--branding" => {
                i += 1;
                branding = Some(PathBuf::from(&args[i]));
            }
            "--sb-key" => {
                i += 1;
                sb_key = Some(PathBuf::from(&args[i]));
            }
            "--preinstall" => {
                i += 1;
                for item in args[i].split(',') {
                    preinstall.push(PathBuf::from(item.trim()));
                }
            }
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(&args[i]));
            }
            other => return Err(format!("Unknown argument: {}", other)),
        }
        i += 1;
    }

    Ok(Opts {
        release_iso: release_iso.ok_or("--release-iso is required")?,
        branding,
        sb_key,
        preinstall,
        output: output.ok_or("--output is required")?,
    })
}

fn print_usage(prog: &str) {
    eprintln!(
        "Usage: {} --release-iso <iso> [--branding <tar.gz>] [--sb-key <cer>] \
         [--preinstall <a.gapp,b.gapp>] --output <img>",
        prog
    );
}

// ─── Build steps ─────────────────────────────────────────────────────────────

fn verify_iso_signature(iso: &Path) {
    let sig = iso.with_extension("sig");
    if sig.exists() {
        println!("[oem-image] Verifying ISO signature: {}", sig.display());
        // In production: call `gpm verify <iso> <sig>` using the release key.
        // For now, log that verification is required.
        println!("[oem-image] WARNING: ISO signature verification not yet wired to HSM key.");
    } else {
        println!("[oem-image] WARNING: No .sig file found for ISO; skipping signature check.");
    }
}

fn extract_iso(iso: &Path, dest: &Path) {
    println!("[oem-image] Extracting ISO to: {}", dest.display());
    fs::create_dir_all(dest).expect("create work dir");

    // On Linux/macOS: `bsdtar xf <iso> -C <dest>` or `7z x`.
    // On Windows: use PowerShell's Expand-Archive or a pre-installed 7-Zip.
    let status = Command::new("7z")
        .args(["x", "-o", &dest.to_string_lossy(), &iso.to_string_lossy()])
        .status()
        .or_else(|_| {
            Command::new("bsdtar")
                .args(["xf", &iso.to_string_lossy(), "-C", &dest.to_string_lossy()])
                .status()
        })
        .expect("extract ISO (needs 7z or bsdtar in PATH)");

    if !status.success() {
        panic!("[oem-image] ISO extraction failed");
    }
}

fn inject_branding(branding: &Path, work_dir: &Path) {
    println!(
        "[oem-image] Injecting branding from: {}",
        branding.display()
    );
    let status = Command::new("tar")
        .args([
            "xzf",
            &branding.to_string_lossy(),
            "-C",
            &work_dir.to_string_lossy(),
        ])
        .status()
        .expect("tar");
    if !status.success() {
        panic!("[oem-image] Branding injection failed");
    }
}

fn enrol_secure_boot_key(key: &Path, work_dir: &Path) {
    println!("[oem-image] Enrolling Secure Boot key: {}", key.display());
    // The EFI NVRAM image lives at <work_dir>/EFI/NVRAM/keys.nvram.
    // Production: call `sbsign` / `cert-to-efi-sig-list` or the GraphOS
    // `gpm sb-enrol` command.
    let nvram_dir = work_dir.join("EFI").join("NVRAM");
    fs::create_dir_all(&nvram_dir).ok();
    let dest = nvram_dir.join("oem-sb-key.cer");
    fs::copy(key, &dest).expect("copy SB key");
    println!("[oem-image] Secure Boot key staged at: {}", dest.display());
}

fn preinstall_app(app: &Path, work_dir: &Path) {
    println!("[oem-image] Pre-installing app: {}", app.display());
    if !app.exists() {
        eprintln!(
            "[oem-image] WARNING: app bundle not found: {}",
            app.display()
        );
        return;
    }
    let pkg_dir = work_dir.join("pkg");
    fs::create_dir_all(&pkg_dir).ok();
    let dest = pkg_dir.join(app.file_name().unwrap());
    fs::copy(app, dest).expect("copy app bundle");
}

fn repack_image(work_dir: &Path, output: &Path) {
    println!("[oem-image] Repacking to GPT image: {}", output.display());
    // Production: use `mkgpt` / `mformat` / custom GPT writer to produce a
    // bootable disk image.  For now, create a tar of the work dir as a
    // placeholder — full GPT image creation requires the partition layout tool.
    let status = Command::new("tar")
        .args([
            "czf",
            &output.to_string_lossy(),
            "-C",
            &work_dir.to_string_lossy(),
            ".",
        ])
        .status()
        .expect("repack");
    if !status.success() {
        panic!("[oem-image] Repack failed");
    }
}
