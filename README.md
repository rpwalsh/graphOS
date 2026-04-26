<!-- Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved. -->

# GraphOS

A ground-up operating system written in Rust. GraphOS boots on bare x86\_64 and
aarch64 hardware via a custom UEFI loader, runs a capability-secured
microkernel, and presents a GPU-composited desktop backed by an on-device
cognitive engine. No LLM. No hallucination. Deterministic inference.
Cryptographically verifiable.

---

## Architecture overview

| Layer | Description |
|---|---|
| **UEFI loader** | Rust no\_std EFI application; hands off a typed `BootInfo` struct to the kernel |
| **Kernel** | Single-binary x86\_64/aarch64; preemptive scheduler, virtual memory, IPC channels, VirtIO drivers, GPU compositor, ACPI/HPET, seccomp-style syscall filtering |
| **Protected ring-3** | Isolated userspace processes (`graphd`, `modeld`, `trainerd`, `compositor`, `servicemgr`, `init`, …) linked against the graph runtime |
| **Graph runtime** | Causal/temporal graph, spectral analytics, BM25 + LSH retrieval, Kneser-Ney language models — all implemented from scratch in safe Rust, air-gapped from the network by default |
| **SDK** | `app-sdk`, `ui-sdk`, `gl-sdk`, `graph-sdk`, `tool-sdk`, `wasm-sdk`, `graphhash` |
| **Apps** | Terminal, Files, Settings, Editor, Browser-lite, AI Console, AI Air Hockey, Shell3D |
| **Tools** | `pkgmgr` (gpm), `appstore` (gps), `cryptsetup`, `bench`, `fuzz`, `oem-image`, `installer` |

## Security model

- **Capability-based IPC** — every cross-process call carries an unforgeable capability token
- **Seccomp allowlist** — each protected binary runs with a minimal syscall whitelist; violations halt the process
- **Ed25519 identity** — all packages are signed; the kernel verifies signatures before loading
- **TPM attestation** — boot chain measurements available via `tpm::attestation`
- **AES-XTS storage** — full-disk encryption via `cryptsetup`
- **KASLR** — kernel base randomised at boot

## Cognitive engine

The on-device inference stack lives entirely in kernel/userspace Rust — no
Python, no ONNX, no external model weights shipped in this repository.
Capabilities include BM25 full-text retrieval, LSH approximate nearest-neighbour,
spectral graph analytics, Kneser-Ney n-gram models, Lanczos eigenvector
decomposition, and a PageRank-style graph walk. All outputs are deterministic
given fixed inputs and are auditable at the byte level.

## Build requirements

- Rust nightly (see `graphos/rust-toolchain.toml`)
- `rust-src`, `llvm-tools-preview`, `rustfmt`, `clippy` components
- QEMU ≥ 8.x and OVMF firmware (for local boot testing)
- PowerShell 7+ (Windows) for build scripts

## Quick start

```powershell
# From graphos/
cargo check -p graphos-kernel --target x86_64-unknown-none \
  -Z build-std=core,alloc,compiler_builtins --features freestanding

# Boot in QEMU (Windows)
.\scripts\boot-windows-qemu.ps1

# Run full verification gates
.\scripts\verify.ps1 -SkipBoot
```

## Scripts

| Script | Purpose |
|---|---|
| `boot.ps1` | Boot entry point (wraps QEMU or bare-metal) |
| `boot-windows-qemu.ps1` | QEMU x86\_64 boot for Windows dev machines |
| `boot-arm64.ps1` | QEMU aarch64 boot |
| `boot-baremetal.ps1` | Write image to bare-metal target |
| `bench.ps1` | Run benchmark suite |
| `release-image.ps1` | Build signed release image |
| `setup-ai-stack.ps1` | Configure on-device cognitive stack |
| `sdk-check.ps1` | SDK build and lint gate |
| `build-icon-atlas.ps1` | Regenerate icon atlas from SVG sources |
| `verify.ps1` | Multi-gate CI smoke test (check → clippy → boot → health) |

## CI

GitHub Actions workflow at `.github/workflows/ci.yml` runs on every push and
pull request to `main` and `release/**` branches:

1. `cargo check` — zero errors, zero warnings
2. `cargo clippy` — all lints treated as errors
3. Host-tool builds (`gpm`, `gps`)
4. QEMU boot smoke with serial health assertions

## License

Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved. See [LICENSE](LICENSE).
