// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::vec::Vec;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let protected_dir = workspace_root.join("userspace").join("protected");
    let app_sdk_dir = workspace_root.join("sdk").join("app-sdk");
    let ui_sdk_dir = workspace_root.join("sdk").join("ui-sdk");
    let assets_config_dir = workspace_root.join("assets").join("config");
    let protected_target_dir = workspace_root.join("target").join("protected-ring3");
    let profile = env::var("PROFILE").expect("profile");
    let protected_profile = "release";
    let git_dir = workspace_root.parent().expect("repo root").join(".git");

    println!("cargo:rerun-if-env-changed=PROFILE");
    println!(
        "cargo:rustc-env=GRAPHOS_BUILD_VERSION={}",
        env::var("CARGO_PKG_VERSION").expect("package version")
    );
    println!("cargo:rustc-env=GRAPHOS_BUILD_PROFILE={profile}");
    if git_dir.is_dir() {
        emit_rerun_for_git_metadata(&git_dir);
    }
    let git_commit = git_short_commit(workspace_root).unwrap_or_else(|| "nogit".to_string());
    let git_dirty = git_dirty_state(workspace_root).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GRAPHOS_BUILD_GIT_SHA={git_commit}");
    println!("cargo:rustc-env=GRAPHOS_BUILD_GIT_DIRTY={git_dirty}");

    println!(
        "cargo:rerun-if-changed={}",
        protected_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        protected_dir.join("src").display()
    );
    emit_rerun_for_tree(&protected_dir.join("src"));
    println!(
        "cargo:rerun-if-changed={}",
        protected_dir.join("bootstrap.graph").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        protected_dir.join("link.ld").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        assets_config_dir.join("modeld.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        protected_dir.join("kernel").join("linker.ld").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        app_sdk_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        app_sdk_dir.join("src").display()
    );
    emit_rerun_for_tree(&app_sdk_dir.join("src"));
    println!(
        "cargo:rerun-if-changed={}",
        ui_sdk_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        ui_sdk_dir.join("src").display()
    );
    emit_rerun_for_tree(&ui_sdk_dir.join("src"));
    if assets_config_dir.is_dir() {
        emit_rerun_for_tree(&assets_config_dir);
    }

    build_protected_services(&protected_dir, &protected_target_dir, protected_profile);

    let profile_dir = protected_target_dir
        .join("x86_64-unknown-none")
        .join(protected_profile);
    let package_store = protected_target_dir.join("graphosp.pkg");
    let package_entry_count = build_package_store(&protected_dir, &profile_dir, &package_store);
    println!("cargo:rustc-env=GRAPHOS_PACKAGE_STORE_ENTRY_COUNT={package_entry_count}");
    println!("cargo:rustc-env=GRAPHOS_PACKAGE_STORE_FORMAT=2");
    println!(
        "cargo:rustc-env=GRAPHOS_PACKAGE_STORE={}",
        package_store.display()
    );
    for (key, bin) in [
        ("GRAPHOS_RING3_INIT_ELF", "init"),
        ("GRAPHOS_RING3_SERVICEMGR_ELF", "servicemgr"),
        ("GRAPHOS_RING3_GRAPHD_ELF", "graphd"),
        ("GRAPHOS_RING3_MODELD_ELF", "modeld"),
        ("GRAPHOS_RING3_TRAINERD_ELF", "trainerd"),
        ("GRAPHOS_RING3_ARTIFACTSD_ELF", "artifactsd"),
        ("GRAPHOS_RING3_SYSD_ELF", "sysd"),
        ("GRAPHOS_RING3_COMPOSITOR_ELF", "compositor"),
        ("GRAPHOS_RING3_SSHD_ELF", "sshd"),
        ("GRAPHOS_RING3_NETD_ELF", "netd"),
    ] {
        let artifact = profile_dir.join(bin);
        if !artifact.is_file() {
            panic!("protected ring3 artifact missing: {}", artifact.display());
        }
        println!("cargo:rustc-env={key}={}", artifact.display());
    }
}

fn emit_rerun_for_git_metadata(git_dir: &Path) {
    for path in [
        git_dir.join("HEAD"),
        git_dir.join("index"),
        git_dir.join("packed-refs"),
    ] {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn emit_rerun_for_tree(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            emit_rerun_for_tree(&path);
        } else if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn build_protected_services(manifest_dir: &Path, target_dir: &Path, profile: &str) {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut command = Command::new(cargo);
    command
        .current_dir(manifest_dir)
        .arg("build")
        .arg("-Z")
        .arg("build-std=core,alloc,compiler_builtins")
        .arg("-Z")
        .arg("build-std-features=compiler-builtins-mem")
        .arg("--target")
        .arg("x86_64-unknown-none")
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--bins");
    command.env_remove("CARGO_ENCODED_RUSTFLAGS");
    command.env_remove("RUSTFLAGS");
    command.env_remove("CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS");
    command.env_remove("RUSTC_WRAPPER");
    command.env_remove("RUSTC_WORKSPACE_WRAPPER");
    // The protected ring3 image is linked at virtual address 0x80_0040_0000
    // (~549 GB), well outside the small/medium code model's ±2 GB window.
    // We need `code-model=large` so the compiler emits 64-bit absolute or
    // RIP-relative-with-64-bit-offset addressing for cross-section accesses
    // (.text → .rodata, etc.); otherwise libcore's formatting helpers
    // generate `R_X86_64_32S` relocations that overflow at link time once
    // the binary grows past a few MB. PIC keeps individual function bodies
    // position-independent.
    let protected_rustflags =
        "-C relocation-model=pic -C code-model=large -C force-frame-pointers=yes";
    command.env("RUSTFLAGS", protected_rustflags);
    command.env(
        "CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS",
        protected_rustflags,
    );
    if profile == "release" {
        command.arg("--release");
    }
    let status = command
        .status()
        .expect("failed to invoke cargo for protected ring3 services");

    if !status.success() {
        panic!("protected ring3 service build failed with status {status}");
    }
}

fn build_package_store(protected_dir: &Path, profile_dir: &Path, output: &Path) -> usize {
    const MAGIC: &[u8; 8] = b"GPKSTORE";
    const VERSION: u32 = 2;
    const HEADER_SIZE: usize = 32;
    const ENTRY_SIZE: usize = 88;
    const IMAGE_SIZE_OFFSET: usize = 16;
    const CHECKSUM_OFFSET: usize = 24;

    enum InputSource {
        File(PathBuf),
        Bytes(Vec<u8>),
    }

    struct Input<'a> {
        store_path: &'a str,
        source: InputSource,
        flags: u32,
    }

    let bootstrap_manifest = fs::read_to_string(protected_dir.join("bootstrap.graph"))
        .unwrap_or_else(|err| panic!("failed to read bootstrap.graph: {err}"));
    let compatibility_manifest = emit_services_txt_compat(&bootstrap_manifest);
    let assets_config_dir = protected_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets")
        .join("config");

    let inputs = [
        Input {
            store_path: "/services/init.elf",
            source: InputSource::File(profile_dir.join("init")),
            flags: 1,
        },
        Input {
            store_path: "/services/servicemgr.elf",
            source: InputSource::File(profile_dir.join("servicemgr")),
            flags: 1,
        },
        Input {
            store_path: "/services/graphd.elf",
            source: InputSource::File(profile_dir.join("graphd")),
            flags: 1,
        },
        Input {
            store_path: "/services/modeld.elf",
            source: InputSource::File(profile_dir.join("modeld")),
            flags: 1,
        },
        Input {
            store_path: "/services/trainerd.elf",
            source: InputSource::File(profile_dir.join("trainerd")),
            flags: 1,
        },
        Input {
            store_path: "/services/artifactsd.elf",
            source: InputSource::File(profile_dir.join("artifactsd")),
            flags: 1,
        },
        Input {
            store_path: "/services/sysd.elf",
            source: InputSource::File(profile_dir.join("sysd")),
            flags: 1,
        },
        Input {
            store_path: "/services/compositor.elf",
            source: InputSource::File(profile_dir.join("compositor")),
            flags: 1,
        },
        Input {
            store_path: "/services/sshd.elf",
            source: InputSource::File(profile_dir.join("sshd")),
            flags: 1,
        },
        Input {
            store_path: "/services/netd.elf",
            source: InputSource::File(profile_dir.join("netd")),
            flags: 1,
        },
        Input {
            store_path: "/config/bootstrap.graph",
            source: InputSource::Bytes(bootstrap_manifest.into_bytes()),
            flags: 0,
        },
        Input {
            store_path: "/config/services.txt",
            source: InputSource::Bytes(compatibility_manifest.into_bytes()),
            flags: 0,
        },
        Input {
            store_path: "/config/modeld.json",
            source: InputSource::File(assets_config_dir.join("modeld.json")),
            flags: 0,
        },
        Input {
            store_path: "/apps/ai-console.elf",
            source: InputSource::File(profile_dir.join("ai-console")),
            flags: 1,
        },
        Input {
            store_path: "/apps/greeter.elf",
            source: InputSource::File(profile_dir.join("greeter")),
            flags: 1,
        },
        Input {
            store_path: "/apps/launcher.elf",
            source: InputSource::File(profile_dir.join("launcher")),
            flags: 1,
        },
        Input {
            store_path: "/apps/cube.elf",
            source: InputSource::File(profile_dir.join("cube")),
            flags: 1,
        },
        Input {
            store_path: "/apps/notepad.elf",
            source: InputSource::File(profile_dir.join("notepad")),
            flags: 1,
        },
        Input {
            store_path: "/apps/calculator.elf",
            source: InputSource::File(profile_dir.join("calculator")),
            flags: 1,
        },
        Input {
            store_path: "/apps/paint.elf",
            source: InputSource::File(profile_dir.join("paint")),
            flags: 1,
        },
        Input {
            store_path: "/apps/terminal.elf",
            source: InputSource::File(profile_dir.join("terminal")),
            flags: 1,
        },
        Input {
            store_path: "/apps/editor.elf",
            source: InputSource::File(profile_dir.join("editor")),
            flags: 1,
        },
        Input {
            store_path: "/apps/files.elf",
            source: InputSource::File(profile_dir.join("files")),
            flags: 1,
        },
        Input {
            store_path: "/apps/display-canary.elf",
            source: InputSource::File(profile_dir.join("display-canary")),
            flags: 1,
        },
        Input {
            store_path: "/apps/ssh.elf",
            source: InputSource::File(profile_dir.join("ssh")),
            flags: 1,
        },
    ];

    let mut seen_paths = BTreeSet::new();
    for input in &inputs {
        if !seen_paths.insert(input.store_path) {
            panic!("duplicate package-store path: {}", input.store_path);
        }
    }

    let mut blobs = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let bytes = match &input.source {
            InputSource::File(path) => fs::read(path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display())),
            InputSource::Bytes(bytes) => bytes.clone(),
        };
        blobs.push(bytes);
    }

    let mut store = Vec::with_capacity(
        HEADER_SIZE + ENTRY_SIZE * inputs.len() + blobs.iter().map(Vec::len).sum::<usize>(),
    );
    store.extend_from_slice(MAGIC);
    store.extend_from_slice(&VERSION.to_le_bytes());
    store.extend_from_slice(&(inputs.len() as u32).to_le_bytes());
    store.extend_from_slice(&0u64.to_le_bytes());
    store.extend_from_slice(&0u64.to_le_bytes());

    let mut data_offset = HEADER_SIZE + ENTRY_SIZE * inputs.len();
    let mut entries = Vec::with_capacity(inputs.len());
    for (input, blob) in inputs.iter().zip(blobs.iter()) {
        data_offset = align_up(data_offset, 16);
        entries.push((input, data_offset, blob.len()));
        data_offset += blob.len();
    }

    for (input, offset, size) in &entries {
        let mut path = [0u8; 64];
        let path_bytes = input.store_path.as_bytes();
        if path_bytes.len() >= path.len() {
            panic!("package-store path too long: {}", input.store_path);
        }
        path[..path_bytes.len()].copy_from_slice(path_bytes);
        store.extend_from_slice(&path);
        store.extend_from_slice(&(*offset as u64).to_le_bytes());
        store.extend_from_slice(&(*size as u64).to_le_bytes());
        store.extend_from_slice(&input.flags.to_le_bytes());
        store.extend_from_slice(&0u32.to_le_bytes());
    }

    for ((_, offset, _), blob) in entries.iter().zip(blobs.iter()) {
        while store.len() < *offset {
            store.push(0);
        }
        store.extend_from_slice(blob);
    }

    let total_size = u64::try_from(store.len()).expect("package store larger than u64");
    store[IMAGE_SIZE_OFFSET..IMAGE_SIZE_OFFSET + 8].copy_from_slice(&total_size.to_le_bytes());
    let checksum = fnv1a64_with_zeroed_checksum(&store, CHECKSUM_OFFSET);
    store[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].copy_from_slice(&checksum.to_le_bytes());

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("create package-store output dir");
    }
    fs::write(output, &store)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", output.display()));
    inputs.len()
}

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

fn fnv1a64_with_zeroed_checksum(bytes: &[u8], checksum_offset: usize) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for (idx, &byte) in bytes.iter().enumerate() {
        let effective = if idx >= checksum_offset && idx < checksum_offset + 8 {
            0
        } else {
            byte
        };
        hash ^= effective as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn git_short_commit(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?;
    let trimmed = sha.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn git_dirty_state(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let dirty = !output.stdout.is_empty();
    Some(if dirty { "dirty" } else { "clean" }.to_string())
}

fn emit_services_txt_compat(manifest: &str) -> String {
    let mut lines = Vec::new();
    for raw_line in manifest.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line == "graph-manifest-v1" {
            continue;
        }

        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 6 || parts[0] != "service" {
            continue;
        }

        if parts[4] == "servicemgr" {
            lines.push(parts[5].to_string());
        }
    }

    let mut compat = lines.join("\n");
    compat.push('\n');
    compat
}
