// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS benchmark suite — IPC latency, syscall overhead, VFS throughput.
//!
//! ## Tests
//! - `ipc`      — round-trip latency via graphos channel syscalls
//! - `syscall`  — raw syscall overhead (SYS_GETPID baseline)
//! - `vfs`      — `/tmp` sequential read/write throughput

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const ITERATIONS: u64 = 100_000;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ns(d: Duration) -> u64 {
    d.as_nanos() as u64
}

fn report(name: &str, iters: u64, total: Duration) {
    let per_iter = ns(total) / iters.max(1);
    println!(
        "{:<20} iters={:<8} total={:.3}ms  per_iter={}ns",
        name,
        iters,
        total.as_secs_f64() * 1000.0,
        per_iter
    );
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_syscall() {
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        // Inline a getpid syscall as the baseline kernel-entry overhead.
        #[cfg(target_os = "linux")]
        unsafe {
            let _pid: i64;
            std::arch::asm!("syscall", in("rax") 39u64, out("rax") _pid, options(nostack));
        }
        #[cfg(not(target_os = "linux"))]
        {
            // On non-Linux hosts just use a noop to measure loop overhead.
            let _ = std::hint::black_box(0u64);
        }
    }
    report("syscall(getpid)", ITERATIONS, start.elapsed());
}

fn bench_vfs_write(path: &str, block_size: usize, blocks: usize) {
    let data = vec![0xABu8; block_size];
    let start = Instant::now();
    let mut f = fs::File::create(path).expect("bench: vfs open failed");
    for _ in 0..blocks {
        f.write_all(&data).expect("bench: vfs write failed");
    }
    drop(f);
    let total = start.elapsed();
    let bytes = (block_size * blocks) as u64;
    let secs = total.as_secs_f64().max(1e-9);
    let mbps = (bytes as f64 / secs) / 1_048_576.0;
    println!(
        "{:<20} block={}B  blocks={}  total={:.3}ms  {:.1} MB/s",
        "vfs_write",
        block_size,
        blocks,
        total.as_secs_f64() * 1000.0,
        mbps
    );
}

fn bench_vfs_read(path: &str, block_size: usize, blocks: usize) {
    let mut buf = vec![0u8; block_size];
    let start = Instant::now();
    let mut f = fs::File::open(path).expect("bench: vfs read open failed");
    for _ in 0..blocks {
        f.read_exact(&mut buf).ok();
    }
    drop(f);
    let total = start.elapsed();
    let bytes = (block_size * blocks) as u64;
    let secs = total.as_secs_f64().max(1e-9);
    let mbps = (bytes as f64 / secs) / 1_048_576.0;
    println!(
        "{:<20} block={}B  blocks={}  total={:.3}ms  {:.1} MB/s",
        "vfs_read",
        block_size,
        blocks,
        total.as_secs_f64() * 1000.0,
        mbps
    );
}

fn bench_ipc() {
    // Without a running GraphOS kernel, measure pipe round-trip as a proxy.
    use std::io::{Read, Write};
    let (mut rx, mut tx): (std::fs::File, std::fs::File) = {
        #[cfg(unix)]
        {
            let mut fds = [0i32; 2];
            unsafe { libc_pipe(&mut fds) };
            (
                unsafe { <std::fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(fds[0]) },
                unsafe { <std::fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(fds[1]) },
            )
        }
        #[cfg(not(unix))]
        {
            // On Windows / non-unix: skip IPC bench.
            println!("{:<20} skipped (not unix)", "ipc_roundtrip");
            return;
        }
    };
    let msg = [0u8; 8];
    let mut reply = [0u8; 8];
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        tx.write_all(&msg).ok();
        rx.read_exact(&mut reply).ok();
    }
    report("ipc_roundtrip", ITERATIONS, start.elapsed());
}

#[cfg(unix)]
extern "C" {
    fn pipe(fds: *mut i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_pipe(fds: &mut [i32; 2]) {
    pipe(fds.as_mut_ptr());
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = env::args().collect();
    let run_all = args.len() < 2;
    let tmp = env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let vfs_path = format!("{}/graphos_bench.tmp", tmp);

    let targets: Vec<&str> = if run_all {
        vec!["syscall", "vfs", "ipc"]
    } else {
        args[1..].iter().map(|s| s.as_str()).collect()
    };

    println!("GraphOS Benchmark Suite");
    println!("{}", "=".repeat(70));

    for t in &targets {
        match *t {
            "syscall" => bench_syscall(),
            "vfs" => {
                bench_vfs_write(&vfs_path, 4096, 1024);
                bench_vfs_read(&vfs_path, 4096, 1024);
                fs::remove_file(&vfs_path).ok();
            }
            "ipc" => bench_ipc(),
            _ => eprintln!("bench: unknown target '{}'", t),
        }
    }
}
