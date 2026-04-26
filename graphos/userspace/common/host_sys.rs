#![allow(dead_code)]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

use std::collections::HashMap;
use std::env;
use std::io::{self, ErrorKind, Write};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{self, Child, Command};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const HOST_IPC_ADDR: &str = "127.0.0.1";
const HOST_IPC_BASE_PORT: u16 = 41000;
const HOST_IPC_MAX_PACKET: usize = 4096;
const HOST_REPLY_ENDPOINT_ENV: &str = "GRAPHOS_HOST_REPLY_ENDPOINT";
const HOST_SERVICE_ENV: &str = "GRAPHOS_HOST_SERVICE";
const HOST_BIN_DIR_ENV: &str = "GRAPHOS_HOST_BIN_DIR";
const SHUTDOWN_TAG: u8 = 0x04;

struct ChildRecord {
    endpoint: u32,
    child: Child,
}

struct HostRuntime {
    recv_sockets: HashMap<u32, UdpSocket>,
    send_socket: Option<UdpSocket>,
    next_pid: u64,
    children: HashMap<u64, ChildRecord>,
}

impl HostRuntime {
    fn new() -> Self {
        Self {
            recv_sockets: HashMap::new(),
            send_socket: None,
            next_pid: 1,
            children: HashMap::new(),
        }
    }
}

static RUNTIME: OnceLock<Mutex<HostRuntime>> = OnceLock::new();

fn runtime() -> &'static Mutex<HostRuntime> {
    RUNTIME.get_or_init(|| Mutex::new(HostRuntime::new()))
}

fn channel_port(channel: u32) -> Option<u16> {
    let port = HOST_IPC_BASE_PORT as u32 + channel;
    u16::try_from(port).ok()
}

fn current_service_name() -> Option<String> {
    if let Ok(service) = env::var(HOST_SERVICE_ENV)
        && !service.is_empty()
    {
        return Some(service);
    }

    let exe = env::current_exe().ok()?;
    let stem = exe.file_stem()?.to_str()?.to_ascii_lowercase();
    Some(stem.strip_prefix("graphos-").unwrap_or(&stem).to_string())
}

fn well_known_endpoint(name: &str) -> u32 {
    match name {
        "servicemgr" => 1,
        "graphd" => 2,
        "modeld" => 3,
        "trainerd" => 4,
        "artifactsd" => 5,
        "sysd" => 6,
        "init" => 63,
        _ => 0,
    }
}

fn current_reply_endpoint() -> u32 {
    if let Ok(raw) = env::var(HOST_REPLY_ENDPOINT_ENV)
        && let Ok(parsed) = raw.parse::<u32>()
    {
        return parsed;
    }

    current_service_name()
        .map(|name| well_known_endpoint(&name))
        .unwrap_or(0)
}

fn bind_recv_socket(channel: u32) -> Option<UdpSocket> {
    let port = channel_port(channel)?;
    let socket = UdpSocket::bind((HOST_IPC_ADDR, port)).ok()?;
    socket.set_nonblocking(true).ok()?;
    Some(socket)
}

fn send_socket(rt: &mut HostRuntime) -> Option<&UdpSocket> {
    if rt.send_socket.is_none() {
        let socket = UdpSocket::bind((HOST_IPC_ADDR, 0)).ok()?;
        let _ = socket.set_nonblocking(true);
        rt.send_socket = Some(socket);
    }
    rt.send_socket.as_ref()
}

fn reap_children(rt: &mut HostRuntime) {
    let mut finished = Vec::new();
    for (&pid, record) in &mut rt.children {
        match record.child.try_wait() {
            Ok(Some(_)) => finished.push(pid),
            Ok(None) => {}
            Err(_) => finished.push(pid),
        }
    }
    for pid in finished {
        rt.children.remove(&pid);
    }
}

fn normalized_name(name: &[u8]) -> Option<String> {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let raw = std::str::from_utf8(&name[..end])
        .ok()?
        .trim()
        .to_ascii_lowercase();
    if raw.is_empty() {
        None
    } else {
        Some(raw.strip_prefix("graphos-").unwrap_or(&raw).to_string())
    }
}

fn logical_binary_name(service: &str) -> String {
    let base = if service.starts_with("graphos-") {
        service.to_string()
    } else {
        format!("graphos-{service}")
    };
    format!("{base}{}", env::consts::EXE_SUFFIX)
}

fn resolve_service_binary(service: &str) -> Option<PathBuf> {
    let binary = logical_binary_name(service);

    if let Ok(dir) = env::var(HOST_BIN_DIR_ENV) {
        let candidate = PathBuf::from(dir).join(&binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let exe_dir = env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = exe_dir.join(binary);
    if candidate.is_file() {
        return Some(candidate);
    }

    None
}

fn send_packet(rt: &mut HostRuntime, channel: u32, tag: u8, reply_endpoint: u32, payload: &[u8]) {
    let Some(port) = channel_port(channel) else {
        return;
    };
    let Some(socket) = send_socket(rt) else {
        return;
    };

    let mut packet = Vec::with_capacity(5 + payload.len());
    packet.push(tag);
    packet.extend_from_slice(&reply_endpoint.to_le_bytes());
    packet.extend_from_slice(payload);

    let _ = socket.send_to(&packet, (HOST_IPC_ADDR, port));
}

fn shutdown_children(rt: &mut HostRuntime) {
    let reply_endpoint = current_reply_endpoint();
    let child_endpoints: Vec<u32> = rt
        .children
        .values()
        .filter_map(|record| (record.endpoint != 0).then_some(record.endpoint))
        .collect();

    for endpoint in child_endpoints {
        send_packet(rt, endpoint, SHUTDOWN_TAG, reply_endpoint, &[]);
    }

    for _ in 0..50 {
        reap_children(rt);
        if rt.children.is_empty() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }

    for record in rt.children.values_mut() {
        let _ = record.child.kill();
        let _ = record.child.wait();
    }
    rt.children.clear();
}

pub fn spawn(name: &[u8]) -> u64 {
    let Some(service) = normalized_name(name) else {
        return 0;
    };
    let Some(path) = resolve_service_binary(&service) else {
        return 0;
    };

    let endpoint = well_known_endpoint(&service);
    let mut cmd = Command::new(&path);
    cmd.env(HOST_SERVICE_ENV, &service);
    if endpoint != 0 {
        cmd.env(HOST_REPLY_ENDPOINT_ENV, endpoint.to_string());
    }
    if let Some(dir) = path.parent() {
        cmd.env(HOST_BIN_DIR_ENV, dir.as_os_str());
    }

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return 0,
    };

    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    reap_children(&mut rt);
    let pid = rt.next_pid;
    rt.next_pid += 1;
    rt.children.insert(pid, ChildRecord { endpoint, child });
    pid
}

pub fn process_alive(pid: u64) -> bool {
    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    reap_children(&mut rt);
    rt.children.contains_key(&pid)
}

pub fn write(fd: u32, data: &[u8]) -> usize {
    let writer: &mut dyn Write = if fd == 2 {
        &mut io::stderr()
    } else {
        &mut io::stdout()
    };

    if writer.write_all(data).is_ok() {
        let _ = writer.flush();
        data.len()
    } else {
        0
    }
}

pub fn exit(code: u32) -> ! {
    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    shutdown_children(&mut rt);
    drop(rt);
    process::exit(code as i32)
}

pub fn channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    if channel == 0 {
        return 0;
    }

    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    reap_children(&mut rt);
    if !rt.recv_sockets.contains_key(&channel) {
        let Some(socket) = bind_recv_socket(channel) else {
            return 0;
        };
        rt.recv_sockets.insert(channel, socket);
    }

    let Some(socket) = rt.recv_sockets.get(&channel) else {
        return 0;
    };

    let mut packet = [0u8; HOST_IPC_MAX_PACKET];
    match socket.recv_from(&mut packet) {
        Ok((len, _)) => {
            if len < 5 {
                return 0;
            }

            let tag = packet[0];
            let reply_endpoint = u32::from_le_bytes([packet[1], packet[2], packet[3], packet[4]]);
            let payload = &packet[5..len];
            let copy_len = payload.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&payload[..copy_len]);

            (copy_len as u64) | ((tag as u64) << 16) | ((reply_endpoint as u64) << 24)
        }
        Err(err) if err.kind() == ErrorKind::WouldBlock => 0,
        Err(_) => 0,
    }
}

pub fn channel_send(channel: u32, payload: &[u8], tag: u8) {
    if channel == 0 {
        return;
    }

    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    reap_children(&mut rt);
    send_packet(&mut rt, channel, tag, current_reply_endpoint(), payload);
}

pub fn yield_now() {
    let mut rt = runtime().lock().expect("host runtime mutex poisoned");
    reap_children(&mut rt);
    drop(rt);
    thread::sleep(Duration::from_millis(1));
}
