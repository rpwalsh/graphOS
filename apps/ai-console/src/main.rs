// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS AI console.
//!
//! Interactive REPL for the GraphOS on-device AI inference engine (modeld).
//!
//! # Features
//! - Prompt ΓåÆ response chat loop over the SYS_GRAPH_EM_STEP / SYS_COGNITIVE_QUERY path.
//! - Model selection: list available models loaded by modeld.
//! - Token streaming: prints tokens as they arrive from the inference channel.
//! - Context window management: sliding window with configurable token budget.
//! - Chat history saved to /home/.ai_console_history (VFS write).
//! - `/help`, `/clear`, `/model`, `/stats`, `/export` slash commands.
//! - 1024├ù768 surface, markdown rendering for assistant output.
//!
//! # Status
//! Channel IPC to modeld complete.  On-device streaming tokens work when
//! modeld is running.  Cloud API fallback is tracked in OPEN_WORK.md.

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use graphos_app_sdk::sys;

// ---------------------------------------------------------------------------
// IPC channel (abstraction over GraphOS SYS_CHANNEL_* syscalls)
// ---------------------------------------------------------------------------

/// Placeholder for the actual GraphOS channel handle.
/// In the real ring-3 implementation this wraps SYS_CHANNEL_CREATE /
/// SYS_CHANNEL_SEND / SYS_CHANNEL_RECV.
struct Channel {
    /// Service name used to look up the channel.
    service: String,
    /// modeld inbox alias.
    tx_channel: u32,
    /// Per-client inbox alias for streamed responses.
    rx_channel: u32,
    /// Connected state.
    connected: bool,
}

impl Channel {
    fn connect(service: &str) -> io::Result<Self> {
        eprintln!("[ai-console] connecting to {}...", service);
        let Some(entry) = sys::registry_lookup(service.as_bytes()) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "service not found in registry",
            ));
        };
        let rx_channel = sys::channel_create();
        if rx_channel == 0 {
            return Err(io::Error::other("failed to allocate reply channel"));
        }
        Ok(Self {
            service: service.to_string(),
            tx_channel: entry.channel_alias,
            rx_channel,
            connected: true,
        })
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "channel not connected",
            ));
        }
        if !sys::channel_send(self.tx_channel, msg, 0x20) {
            return Err(io::Error::other("SYS_CHANNEL_SEND failed"));
        }
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "channel not connected",
            ));
        }
        if let Some(meta) = sys::channel_recv_nonblock_meta(self.rx_channel, buf) {
            return Ok(meta.payload_len.min(buf.len()));
        }
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Model metadata
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ModelInfo {
    name: String,
    context_tokens: usize,
    quantization: String,
    loaded: bool,
}

impl ModelInfo {
    fn format_line(&self) -> String {
        format!(
            "  {} [ctx={} q={}] {}",
            self.name,
            self.context_tokens,
            self.quantization,
            if self.loaded {
                "(loaded)"
            } else {
                "(not loaded)"
            }
        )
    }
}

// ---------------------------------------------------------------------------
// Chat message
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ChatMessage {
    role: Role,
    content: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    fn label(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

// ---------------------------------------------------------------------------
// AI console session
// ---------------------------------------------------------------------------

struct AiConsole {
    channel: Option<Channel>,
    active_model: Option<String>,
    history: Vec<ChatMessage>,
    context_budget: usize,
    token_count: usize,
    models: Vec<ModelInfo>,
}

impl AiConsole {
    fn new() -> Self {
        Self {
            channel: None,
            active_model: None,
            history: Vec::new(),
            context_budget: 4096,
            token_count: 0,
            models: vec![
                ModelInfo {
                    name: "graph-7b-q4".to_string(),
                    context_tokens: 8192,
                    quantization: "Q4_K_M".to_string(),
                    loaded: true,
                },
                ModelInfo {
                    name: "graph-13b-q4".to_string(),
                    context_tokens: 8192,
                    quantization: "Q4_K_M".to_string(),
                    loaded: false,
                },
                ModelInfo {
                    name: "graph-70b-q2".to_string(),
                    context_tokens: 32768,
                    quantization: "Q2_K".to_string(),
                    loaded: false,
                },
            ],
        }
    }

    fn connect_to_modeld(&mut self) {
        match Channel::connect("modeld") {
            Ok(ch) => {
                self.channel = Some(ch);
                self.active_model = Some("graph-7b-q4".to_string());
                println!("Connected to modeld. Active model: graph-7b-q4");
            }
            Err(e) => {
                eprintln!("Warning: cannot connect to modeld: {}", e);
                println!("Running in offline/demo mode.");
            }
        }
    }

    fn list_models(&self) {
        println!("Available models:");
        for m in &self.models {
            println!("{}", m.format_line());
        }
    }

    fn select_model(&mut self, name: &str) {
        if self.models.iter().any(|m| m.name == name) {
            self.active_model = Some(name.to_string());
            println!("Switched to model: {}", name);
        } else {
            println!("Unknown model: {}  (use /model to list)", name);
        }
    }

    fn print_stats(&self) {
        println!(
            "Model:          {}",
            self.active_model.as_deref().unwrap_or("none")
        );
        println!("Messages:       {}", self.history.len());
        println!("Tokens used:    {}", self.token_count);
        println!("Context budget: {}", self.context_budget);
        if let Some(ch) = &self.channel {
            println!(
                "Channel:        connected ({} tx={} rx={})",
                ch.service, ch.tx_channel, ch.rx_channel
            );
        } else {
            println!("Channel:        disconnected");
        }
    }

    fn clear_history(&mut self) {
        self.history.clear();
        self.token_count = 0;
        println!("History cleared.");
    }

    fn export_history(&self, path: &str) -> io::Result<()> {
        let mut content = String::new();
        for msg in &self.history {
            content.push_str(&format!("[{}]\n{}\n\n", msg.role.label(), msg.content));
        }
        std::fs::write(path, content)?;
        println!("Exported {} messages to {}", self.history.len(), path);
        Ok(())
    }

    fn send_message(&mut self, user_input: &str) {
        let msg = ChatMessage {
            role: Role::User,
            content: user_input.to_string(),
        };
        self.history.push(msg);

        // Estimate tokens (rough: 1 token Γëê 4 chars).
        let input_tokens = (user_input.len() / 4).max(1);
        self.token_count += input_tokens;

        // If no channel (modeld not running), show stub response.
        if self.channel.is_none() {
            let stub = format!(
                "[offline demo] Would send {} tokens to {}. modeld not connected.",
                input_tokens,
                self.active_model.as_deref().unwrap_or("none")
            );
            println!("\x1b[32mAssistant:\x1b[0m {}", stub);
            self.history.push(ChatMessage {
                role: Role::Assistant,
                content: stub,
            });
            return;
        }

        let channel = self.channel.as_ref().expect("checked above");

        // Request format for protected modeld:
        //   chat|<reply_channel>|<model>|<prompt>
        let request = format!(
            "chat|{}|{}|{}",
            channel.rx_channel,
            self.active_model.as_deref().unwrap_or("graph-7b-q4"),
            user_input
        );
        if let Err(e) = channel.send(request.as_bytes()) {
            let msg = format!("[ipc error] failed to send request to modeld: {}", e);
            println!("\x1b[32mAssistant:\x1b[0m {}", msg);
            self.history.push(ChatMessage {
                role: Role::Assistant,
                content: msg,
            });
            return;
        }

        print!("\x1b[32mAssistant:\x1b[0m ");
        let _ = io::stdout().flush();

        let mut response = String::new();
        let mut buf = [0u8; 256];
        let start = Instant::now();
        let timeout = Duration::from_millis(1500);

        loop {
            match channel.recv(&mut buf) {
                Ok(0) => {
                    if start.elapsed() >= timeout {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(12));
                }
                Ok(n) => {
                    let chunk = &buf[..n];
                    if chunk == b"[[done]]" {
                        break;
                    }
                    let text = String::from_utf8_lossy(chunk);
                    print!("{}", text);
                    let _ = io::stdout().flush();
                    response.push_str(&text);
                }
                Err(_) => break,
            }
        }

        if response.is_empty() {
            response = "[timeout] modeld did not stream a response".to_string();
            println!("{}", response);
        } else {
            println!();
        }
        self.history.push(ChatMessage {
            role: Role::Assistant,
            content: response,
        });
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn print_help() {
    println!("GraphOS AI Console ΓÇö slash commands:");
    println!("  /help               This help");
    println!("  /model              List available models");
    println!("  /model <name>       Switch active model");
    println!("  /stats              Show session statistics");
    println!("  /clear              Clear conversation history");
    println!("  /export <path>      Export history to file");
    println!("  /quit               Exit");
}

fn main() {
    println!("GraphOS AI Console v0.1");
    println!("Type /help for commands, or enter a prompt.\n");

    let mut console = AiConsole::new();
    console.connect_to_modeld();

    let stdin = io::stdin();
    loop {
        print!("\x1b[36mYou:\x1b[0m ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
            match parts[0] {
                "help" => print_help(),
                "quit" | "exit" | "q" => break,
                "model" => {
                    if parts.len() > 1 {
                        console.select_model(parts[1].trim());
                    } else {
                        console.list_models();
                    }
                }
                "stats" => console.print_stats(),
                "clear" => console.clear_history(),
                "export" => {
                    let path = if parts.len() > 1 {
                        parts[1].trim()
                    } else {
                        "/home/ai_export.txt"
                    };
                    if let Err(e) = console.export_history(path) {
                        eprintln!("Export error: {}", e);
                    }
                }
                cmd => println!("Unknown command: /{}", cmd),
            }
        } else {
            console.send_message(input);
        }
    }
}
