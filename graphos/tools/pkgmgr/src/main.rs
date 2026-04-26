// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! gpm — GraphOS Package Manager
//!
//! Subcommands:
//!   keygen          Generate a new ed25519 signing key pair
//!   build           Bundle a driver ELF + manifest into a .gpkg archive
//!   sign            Sign an existing .gpkg with a private key
//!   verify          Verify a .gpkg signature against a public key
//!   install         Install a signed .gpkg to a target root (via VirtFS image)
//!   remove          Remove a package by UUID from a target root

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "gpm", version, about = "GraphOS Package Manager")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a new ed25519 key pair (prints hex to stdout)
    Keygen {
        /// Write private key to this file
        #[arg(long, default_value = "gpm-sign.key")]
        privkey: PathBuf,
        /// Write public key to this file
        #[arg(long, default_value = "gpm-sign.pub")]
        pubkey: PathBuf,
    },
    /// Build a .gpkg archive from a manifest JSON + driver ELF
    Build {
        /// Path to the JSON manifest file
        #[arg(long)]
        manifest: PathBuf,
        /// Path to the signed driver ELF binary
        #[arg(long)]
        driver: PathBuf,
        /// Output .gpkg path
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign a .gpkg archive (appends .sig sidecar)
    Sign {
        /// .gpkg to sign
        package: PathBuf,
        /// Private key file (hex, 32 bytes)
        #[arg(long)]
        privkey: PathBuf,
    },
    /// Verify a .gpkg signature
    Verify {
        /// .gpkg archive
        package: PathBuf,
        /// Public key file (hex, 32 bytes)
        #[arg(long)]
        pubkey: PathBuf,
    },
    /// Install a signed .gpkg into a target root directory
    Install {
        /// .gpkg archive
        package: PathBuf,
        /// Public key used to verify the signature
        #[arg(long)]
        pubkey: PathBuf,
        /// Target root directory (e.g. path to mounted VirtFS image)
        #[arg(long, default_value = "./rootfs")]
        root: PathBuf,
    },
    /// Remove an installed package by UUID
    Remove {
        /// Package UUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)
        uuid: String,
        /// Target root directory
        #[arg(long, default_value = "./rootfs")]
        root: PathBuf,
    },
}

// ─── Manifest ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct PackageManifest {
    /// UUID v4 identifying this package (xxxxxxxx-…)
    package_uuid: String,
    /// Human-readable name
    name: String,
    /// Semver version string
    version: String,
    /// Target PCI device UUID (matches DeviceUuid in the kernel)
    target_device_uuid: String,
    /// Dependencies as package UUIDs
    #[serde(default)]
    dependencies: Vec<String>,
    /// SHA-256 hex digest of the driver ELF (computed at build time)
    driver_sha256: String,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn read_hex_key_32(path: &Path) -> Result<[u8; 32]> {
    let s =
        fs::read_to_string(path).with_context(|| format!("reading key file {}", path.display()))?;
    let bytes = hex::decode(s.trim())
        .with_context(|| format!("decoding hex key from {}", path.display()))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("key must be exactly 32 bytes"))
}

fn sig_path(pkg: &Path) -> PathBuf {
    let mut p = pkg.to_path_buf();
    let ext = p
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    p.set_extension(format!("{ext}.sig"));
    p
}

// ─── Subcommand handlers ──────────────────────────────────────────────────────

fn cmd_keygen(privkey: &Path, pubkey: &Path) -> Result<()> {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    fs::write(privkey, hex::encode(signing_key.to_bytes()))
        .with_context(|| format!("writing private key to {}", privkey.display()))?;
    fs::write(pubkey, hex::encode(verifying_key.to_bytes()))
        .with_context(|| format!("writing public key to {}", pubkey.display()))?;
    println!("Generated key pair:");
    println!("  privkey: {}", privkey.display());
    println!("  pubkey:  {}", pubkey.display());
    println!("  pub hex: {}", hex::encode(verifying_key.to_bytes()));
    Ok(())
}

fn cmd_build(manifest_path: &Path, driver_path: &Path, out: &Path) -> Result<()> {
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("reading manifest {}", manifest_path.display()))?;
    let mut manifest: PackageManifest =
        serde_json::from_slice(&manifest_bytes).with_context(|| "parsing manifest JSON")?;
    let driver_bytes = fs::read(driver_path)
        .with_context(|| format!("reading driver ELF {}", driver_path.display()))?;

    // Compute and embed driver SHA-256.
    let digest = hex::encode(Sha256::digest(&driver_bytes));
    manifest.driver_sha256 = digest.clone();
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    // Pack into a gzip-compressed tar archive.
    let file = fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut ar = tar::Builder::new(enc);

    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    ar.append_data(&mut header, "manifest.json", manifest_bytes.as_slice())?;

    let mut header2 = tar::Header::new_gnu();
    header2.set_size(driver_bytes.len() as u64);
    header2.set_mode(0o755);
    header2.set_cksum();
    ar.append_data(&mut header2, "driver.elf", driver_bytes.as_slice())?;

    ar.finish()?;
    println!("Built {} (driver sha256={})", out.display(), digest);
    Ok(())
}

fn cmd_sign(package: &Path, privkey_path: &Path) -> Result<()> {
    let raw_key = read_hex_key_32(privkey_path)?;
    let signing_key = SigningKey::from_bytes(&raw_key);
    let pkg_bytes = fs::read(package).with_context(|| format!("reading {}", package.display()))?;
    let sig: Signature = signing_key.sign(&pkg_bytes);
    let sig_file = sig_path(package);
    fs::write(&sig_file, hex::encode(sig.to_bytes()))
        .with_context(|| format!("writing signature to {}", sig_file.display()))?;
    println!("Signed: {}", sig_file.display());
    Ok(())
}

fn cmd_verify(package: &Path, pubkey_path: &Path) -> Result<()> {
    let raw_pub = read_hex_key_32(pubkey_path)?;
    let verifying_key = VerifyingKey::from_bytes(&raw_pub)?;
    let pkg_bytes = fs::read(package).with_context(|| format!("reading {}", package.display()))?;
    let sig_file = sig_path(package);
    let sig_hex = fs::read_to_string(&sig_file)
        .with_context(|| format!("reading signature {}", sig_file.display()))?;
    let sig_bytes: [u8; 64] = hex::decode(sig_hex.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    verifying_key
        .verify(&pkg_bytes, &sig)
        .context("signature verification FAILED")?;
    println!("OK: signature is valid");
    Ok(())
}

fn cmd_install(package: &Path, pubkey_path: &Path, root: &Path) -> Result<()> {
    // 1. Verify signature.
    cmd_verify(package, pubkey_path)?;

    // 2. Unpack archive.
    let pkg_bytes = fs::read(package)?;
    let dec = flate2::read::GzDecoder::new(pkg_bytes.as_slice());
    let mut ar = tar::Archive::new(dec);

    let pkg_dir = root.join("pkg").join("drivers");
    fs::create_dir_all(&pkg_dir)?;

    let mut manifest_opt: Option<PackageManifest> = None;

    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let dest = pkg_dir.join(&name);
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        if name == "manifest.json" {
            manifest_opt = Some(serde_json::from_slice(&data)?);
        }
        fs::write(&dest, &data).with_context(|| format!("writing {}", dest.display()))?;
    }

    let manifest = manifest_opt.context("archive missing manifest.json")?;
    println!(
        "Installed {} v{} (uuid={}) -> {}",
        manifest.name,
        manifest.version,
        manifest.package_uuid,
        pkg_dir.display()
    );
    Ok(())
}

fn cmd_remove(uuid: &str, root: &Path) -> Result<()> {
    let pkg_dir = root.join("pkg").join("drivers");
    if !pkg_dir.exists() {
        bail!("No packages installed at {}", root.display());
    }
    // Find manifest.json files whose package_uuid matches.
    let mut removed = 0usize;
    for entry in fs::read_dir(&pkg_dir)? {
        let entry = entry?;
        if entry.file_name() == "manifest.json" {
            let data = fs::read(entry.path())?;
            if let Ok(m) = serde_json::from_slice::<PackageManifest>(&data) {
                if m.package_uuid == uuid {
                    // Remove the parent directory subtree.
                    let parent = entry.path().parent().unwrap().to_path_buf();
                    fs::remove_dir_all(&parent)?;
                    println!("Removed {} from {}", m.name, parent.display());
                    removed += 1;
                }
            }
        }
    }
    if removed == 0 {
        bail!("Package with UUID {} not found", uuid);
    }
    Ok(())
}

// ─── main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Cmd::Keygen { privkey, pubkey } => cmd_keygen(privkey, pubkey),
        Cmd::Build {
            manifest,
            driver,
            out,
        } => cmd_build(manifest, driver, out),
        Cmd::Sign { package, privkey } => cmd_sign(package, privkey),
        Cmd::Verify { package, pubkey } => cmd_verify(package, pubkey),
        Cmd::Install {
            package,
            pubkey,
            root,
        } => cmd_install(package, pubkey, root),
        Cmd::Remove { uuid, root } => cmd_remove(uuid, root),
    }
}
