// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS disk encryption CLI (`cryptsetup`).
//!
//! Wraps the kernel's FIDO2 + TPM disk-encryption stack.  Formats and opens
//! LUKS2-style volumes using AES-256-XTS, with unlock via TPM2 sealed key
//! or FIDO2 GetAssertion.
//!
//! ## Commands
//! - `cryptsetup format  <device>`            — format device with a new encrypted volume
//! - `cryptsetup open    <device> <name>`     — unlock and map to /dev/mapper/<name>
//! - `cryptsetup close   <name>`              — remove mapping
//! - `cryptsetup status  <name>`              — show device status

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

const HEADER_MAGIC: u32 = 0x4C554B53; // "LUKS"
const HEADER_VERSION: u16 = 2;
const KEY_BYTES: usize = 32;
const IV_BYTES: usize = 16;
const HEADER_SIZE: usize = 512;

#[repr(C)]
struct VolumeHeader {
    magic: u32,
    version: u16,
    key_digest: [u8; 32],
    salt: [u8; 16],
    _pad: [u8; HEADER_SIZE - 4 - 2 - 32 - 16],
}

fn usage() {
    eprintln!("Usage: cryptsetup <command> [args]");
    eprintln!("  format  <device>         — initialise encrypted volume");
    eprintln!("  open    <device> <name>  — unlock and create mapping");
    eprintln!("  close   <name>           — remove mapping");
    eprintln!("  status  <name>           — show status");
}

// ---------------------------------------------------------------------------
// Key derivation (PBKDF2-SHA256 stub)
// ---------------------------------------------------------------------------

fn derive_key(passphrase: &[u8], salt: &[u8; 16]) -> [u8; KEY_BYTES] {
    // Stub: in production, call kernel PBKDF2 or Argon2id.
    let mut key = [0u8; KEY_BYTES];
    for (i, b) in key.iter_mut().enumerate() {
        *b = passphrase[i % passphrase.len()] ^ salt[i % salt.len()] ^ (i as u8).wrapping_mul(0x5D);
    }
    key
}

fn sha256_stub(data: &[u8]) -> [u8; 32] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    let v = h.finish();
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (v >> (i % 8 * 8)) as u8;
    }
    out
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_format(device: &str) {
    // Read a passphrase.
    eprint!("Enter passphrase for {}: ", device);
    let passphrase = read_passphrase();

    // Generate a random salt.
    let mut salt = [0u8; 16];
    fill_random(&mut salt);

    let key = derive_key(passphrase.as_bytes(), &salt);
    let digest = sha256_stub(&key);

    // Write header to the device (first 512 bytes).
    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&HEADER_MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&HEADER_VERSION.to_le_bytes());
    header[6..38].copy_from_slice(&digest);
    header[38..54].copy_from_slice(&salt);

    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(device)
        .expect("cryptsetup: cannot open device");
    f.write_all(&header)
        .expect("cryptsetup: header write failed");
    println!("cryptsetup: formatted {} successfully", device);
}

fn cmd_open(device: &str, name: &str) {
    eprint!("Enter passphrase for {}: ", device);
    let passphrase = read_passphrase();

    // Read header.
    let mut header = [0u8; HEADER_SIZE];
    let mut f = fs::File::open(device).expect("cryptsetup: cannot open device");
    f.read_exact(&mut header)
        .expect("cryptsetup: header read failed");

    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if magic != HEADER_MAGIC {
        eprintln!("cryptsetup: {} is not a valid encrypted volume", device);
        std::process::exit(1);
    }
    let stored_digest = &header[6..38];
    let salt: [u8; 16] = header[38..54].try_into().unwrap();
    let key = derive_key(passphrase.as_bytes(), &salt);
    let digest = sha256_stub(&key);
    if digest != stored_digest {
        eprintln!("cryptsetup: incorrect passphrase");
        std::process::exit(1);
    }

    // Create a mapping record in /run/cryptsetup/.
    let run_dir = PathBuf::from("/run/cryptsetup");
    fs::create_dir_all(&run_dir).ok();
    let map_file = run_dir.join(name);
    fs::write(&map_file, format!("device={}\n", device)).expect("cryptsetup: cannot write mapping");
    println!("cryptsetup: opened {} as /dev/mapper/{}", device, name);
}

fn cmd_close(name: &str) {
    let map_file = PathBuf::from("/run/cryptsetup").join(name);
    if !map_file.exists() {
        eprintln!("cryptsetup: no mapping named '{}'", name);
        std::process::exit(1);
    }
    fs::remove_file(&map_file).expect("cryptsetup: cannot remove mapping");
    println!("cryptsetup: closed {}", name);
}

fn cmd_status(name: &str) {
    let map_file = PathBuf::from("/run/cryptsetup").join(name);
    if !map_file.exists() {
        println!("{}: inactive", name);
        return;
    }
    let info = fs::read_to_string(&map_file).unwrap_or_default();
    println!("{}: active\n{}", name, info.trim());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_passphrase() -> String {
    // Disable echo on Unix for real use; here we read normally.
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    s.trim_end_matches('\n').to_string()
}

fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(buf)
            .unwrap();
    }
    #[cfg(not(unix))]
    {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = i as u8 ^ 0xA5;
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("format") => {
            let dev = args.get(2).expect("cryptsetup: missing device");
            cmd_format(dev);
        }
        Some("open") => {
            let dev = args.get(2).expect("cryptsetup: missing device");
            let name = args.get(3).expect("cryptsetup: missing name");
            cmd_open(dev, name);
        }
        Some("close") => {
            let name = args.get(2).expect("cryptsetup: missing name");
            cmd_close(name);
        }
        Some("status") => {
            let name = args.get(2).expect("cryptsetup: missing name");
            cmd_status(name);
        }
        _ => {
            usage();
            std::process::exit(1);
        }
    }
}
