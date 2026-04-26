// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

struct OracleConfig {
    required_markers: Vec<String>,
    fail_markers: Vec<String>,
    forbidden_markers: Vec<String>,
    timeout_secs: u64,
}

fn parse_csv_env_or(default_value: &str, key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_else(|_| default_value.to_string())
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
}

fn run_qemu_serial_oracle(config: OracleConfig) {
    let Some(command_line) = std::env::var("GRAPHOS_E2E_CMD").ok() else {
        eprintln!("skipping: GRAPHOS_E2E_CMD is not set");
        return;
    };

    let mut parts = command_line.split_whitespace();
    let Some(program) = parts.next() else {
        panic!("GRAPHOS_E2E_CMD is empty");
    };
    let args = parts.collect::<Vec<_>>();

    let mut child = Command::new(program)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn QEMU command");

    let stdout = child.stdout.take().expect("missing stdout pipe");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(text) = line {
                let _ = tx.send(text);
            }
        }
    });

    let mut found_required = vec![false; config.required_markers.len()];
    let deadline = Instant::now() + Duration::from_secs(config.timeout_secs);
    let mut recent = Vec::new();

    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!(
                "timeout waiting for required markers; recent output: {:?}",
                recent
            );
        }

        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "QEMU exited before required markers with status {status}; recent output: {:?}",
                recent
            );
        }

        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if recent.len() >= 32 {
                    recent.remove(0);
                }
                recent.push(line.clone());

                if config
                    .fail_markers
                    .iter()
                    .any(|marker| line.contains(marker))
                {
                    let _ = child.kill();
                    panic!("saw failure marker in serial output: {line}");
                }

                if config
                    .forbidden_markers
                    .iter()
                    .any(|marker| line.contains(marker))
                {
                    let _ = child.kill();
                    panic!("saw forbidden marker in serial output: {line}");
                }

                for (idx, marker) in config.required_markers.iter().enumerate() {
                    if line.contains(marker) {
                        found_required[idx] = true;
                    }
                }

                if found_required.iter().all(|found| *found) {
                    let _ = child.kill();
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(Some(status)) = child.try_wait() {
                    panic!(
                        "serial stream ended before required markers; process exited with {status}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires a configured QEMU command"]
fn e2e_boot_reaches_ready_marker_without_panic() {
    let ready_marker = std::env::var("GRAPHOS_E2E_READY_MARKER")
        .unwrap_or_else(|_| "[desktop] host loop started".to_string());
    let fail_markers = parse_csv_env_or("panic,[panic],FAIL", "GRAPHOS_E2E_FAIL_MARKERS");
    let timeout_secs = std::env::var("GRAPHOS_E2E_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);

    run_qemu_serial_oracle(OracleConfig {
        required_markers: vec![ready_marker],
        fail_markers,
        forbidden_markers: Vec::new(),
        timeout_secs,
    });
}

#[test]
#[ignore = "requires a configured QEMU command"]
fn e2e_desktop_cube_boots_without_compositor_takeover() {
    let fail_markers = parse_csv_env_or("panic,[panic],FAIL", "GRAPHOS_E2E_FAIL_MARKERS");
    let timeout_secs = std::env::var("GRAPHOS_E2E_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);

    run_qemu_serial_oracle(OracleConfig {
        required_markers: vec![
            "[uinit] launching boot cube".to_string(),
            "[desktop] host loop started".to_string(),
        ],
        fail_markers,
        forbidden_markers: vec!["[userland] resolve compositor".to_string()],
        timeout_secs,
    });
}
