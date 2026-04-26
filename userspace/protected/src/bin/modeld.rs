#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![no_main]

#[path = "../runtime.rs"]
mod runtime;
#[path = "../workspace_context.rs"]
mod workspace_context;

use core::panic::PanicInfo;

const TAG_INFERENCE_REQUEST: u8 = 0x20;
const TAG_INFERENCE_RESPONSE: u8 = 0x21;
const TAG_SERVICE_STATUS: u8 = 0x31;

const CONFIG_PATHS: [&[u8]; 3] = [
    b"/persist/modeld.json",
    b"/data/etc/graphos/modeld.json",
    b"/pkg/config/modeld.json",
];
const DEFAULT_HOST: [u8; 4] = [10, 0, 2, 2];
const SERVICE_NAMES: [&[u8]; 7] = [
    b"graphd",
    b"servicemgr",
    b"sysd",
    b"trainerd",
    b"artifactsd",
    b"terminal",
    b"compositor",
];
const CONFIG_CAP: usize = 4096;
const DOCTRINE_CAP: usize = 2048;
const AUGMENTED_PROMPT_CAP: usize = 2400;
const REQUEST_BODY_CAP: usize = 3584;
const HTTP_RESPONSE_CAP: usize = 8192;
const RESPONSE_CAP: usize = 3072;
const STREAM_CHUNK: usize = 88;

const FALLBACK_DOCTRINE: &[u8] = b"\
SCCE is the graph-first synthesis stack backed by the local Walsh Technical Group codebase.\n\
CastleHale demos are the proof deck for PowerWalk / WTG predictive math.\n\
heterogeneousTemporalWalkEmbeddings.zip is the canonical temporal walk corpus for the predictive classifier lane.\n\
Walsh-Hadamard transforms are separate from the Walsh Technical Group predictive classifier math.\n\
Self-healing means graph-observed context, predictive hints, bounded recovery, and provenance-first operator loops.\n";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Auto,
    Builtin,
    Scce,
    Ollama,
}

#[derive(Clone, Copy)]
struct ModelConfig {
    backend: Backend,
    scce_host: [u8; 4],
    scce_port: u16,
    ollama_host: [u8; 4],
    ollama_port: u16,
    ollama_model: [u8; 32],
    ollama_model_len: usize,
    doctrine: [u8; DOCTRINE_CAP],
    doctrine_len: usize,
}

impl ModelConfig {
    const fn default() -> Self {
        Self {
            backend: Backend::Auto,
            scce_host: DEFAULT_HOST,
            scce_port: 3000,
            ollama_host: DEFAULT_HOST,
            ollama_port: 11434,
            ollama_model: [
                b'g', b'e', b'm', b'm', b'a', b'3', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            ollama_model_len: 6,
            doctrine: [0; DOCTRINE_CAP],
            doctrine_len: 0,
        }
    }

    fn ollama_model(&self) -> &[u8] {
        &self.ollama_model[..self.ollama_model_len]
    }

    fn doctrine(&self) -> &[u8] {
        if self.doctrine_len == 0 {
            FALLBACK_DOCTRINE
        } else {
            &self.doctrine[..self.doctrine_len]
        }
    }
}

#[derive(Clone, Copy)]
struct ChatRequest<'a> {
    reply_channel: u32,
    model: &'a [u8],
    prompt: &'a [u8],
}

#[derive(Clone, Copy)]
struct Route<'a> {
    backend: Backend,
    ollama_model: &'a [u8],
}

#[derive(Clone, Copy)]
struct BackendFailure {
    backend: Backend,
    host: [u8; 4],
    port: u16,
}

#[derive(Clone, Copy)]
struct HttpResult {
    status: u16,
    body_len: usize,
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let resolved_channel = runtime::service_inbox_or_die(b"modeld");
    runtime::claim_inbox(resolved_channel);
    runtime::write_line(b"[modeld] protected inference salon online\n");
    let _ = runtime::bootstrap_named_status(b"service-ready:", b"modeld");
    let _ = runtime::bootstrap_named_status(b"service-bound:", b"modeld");
    runtime::announce_service_ready(b"modeld");

    let mut inbox = [0u8; 256];
    loop {
        let raw = runtime::channel_recv(resolved_channel, &mut inbox);
        if raw == u64::MAX {
            runtime::yield_now();
            continue;
        }

        let payload_len = (raw & 0xFFFF) as usize;
        let tag = ((raw >> 16) & 0xFF) as u8;
        let reply_endpoint = (raw >> 24) as u32;
        if payload_len == 0 || payload_len > inbox.len() {
            runtime::yield_now();
            continue;
        }
        let payload = &inbox[..payload_len];

        if payload == b"shutdown" {
            let _ = runtime::bootstrap_named_status(b"service-stop:", b"modeld");
            runtime::write_line(b"[modeld] shutdown\n");
            runtime::exit(0);
        }

        if tag == TAG_INFERENCE_REQUEST || tag == 0x00 {
            if let Some(request) = parse_chat_request(payload) {
                handle_chat_request(request);
                continue;
            }
        }

        if reply_endpoint != 0 {
            let _ = runtime::channel_send(reply_endpoint, b"ack", TAG_SERVICE_STATUS);
        }
    }
}

fn handle_chat_request(request: ChatRequest<'_>) {
    if request.reply_channel == 0 {
        return;
    }

    let config = load_config();
    let route = select_route(&config, request.model);
    let mut body = [0u8; RESPONSE_CAP];
    if let Some((backend, len)) = try_local_command(request.prompt, route, &config, &mut body) {
        stream_reply(request.reply_channel, backend_label(backend), &body[..len]);
        return;
    }
    let (backend, len) = run_backend(route, request.prompt, &config, &mut body);
    stream_reply(request.reply_channel, backend_label(backend), &body[..len]);
}

fn try_local_command(
    prompt: &[u8],
    route: Route<'_>,
    config: &ModelConfig,
    out: &mut [u8],
) -> Option<(Backend, usize)> {
    let prompt = trim_ascii(prompt);
    if matches_local_command(prompt, b"/ai status")
        || matches_local_command(prompt, b"ai status")
        || matches_local_command(prompt, b"/modeld status")
        || matches_local_command(prompt, b"modeld status")
        || matches_local_command(prompt, b"/backend status")
        || matches_local_command(prompt, b"backend status")
    {
        return Some((Backend::Builtin, compose_status_reply(route, config, out)));
    }

    if matches_local_command(prompt, b"/ai help")
        || matches_local_command(prompt, b"ai help")
        || matches_local_command(prompt, b"/modeld help")
        || matches_local_command(prompt, b"modeld help")
    {
        return Some((Backend::Builtin, compose_help_reply(config, out)));
    }

    None
}

fn matches_local_command(prompt: &[u8], command: &[u8]) -> bool {
    eq_ignore_ascii_case(trim_ascii(prompt), command)
}

fn run_backend(
    route: Route<'_>,
    prompt: &[u8],
    config: &ModelConfig,
    out: &mut [u8],
) -> (Backend, usize) {
    match route.backend {
        Backend::Builtin => (
            Backend::Builtin,
            compose_builtin_reply(prompt, config, None, out),
        ),
        Backend::Scce => {
            if let Some(len) = query_scce(config, prompt, out) {
                (Backend::Scce, len)
            } else {
                let failure = BackendFailure {
                    backend: Backend::Scce,
                    host: config.scce_host,
                    port: config.scce_port,
                };
                (
                    Backend::Builtin,
                    compose_builtin_reply(prompt, config, Some(failure), out),
                )
            }
        }
        Backend::Ollama => {
            if let Some(len) = query_ollama(config, route.ollama_model, prompt, out) {
                (Backend::Ollama, len)
            } else {
                let failure = BackendFailure {
                    backend: Backend::Ollama,
                    host: config.ollama_host,
                    port: config.ollama_port,
                };
                (
                    Backend::Builtin,
                    compose_builtin_reply(prompt, config, Some(failure), out),
                )
            }
        }
        Backend::Auto => {
            if let Some(len) = query_scce(config, prompt, out) {
                return (Backend::Scce, len);
            }
            if let Some(len) = query_ollama(config, route.ollama_model, prompt, out) {
                return (Backend::Ollama, len);
            }
            (
                Backend::Builtin,
                compose_builtin_reply(
                    prompt,
                    config,
                    Some(BackendFailure {
                        backend: Backend::Auto,
                        host: [0, 0, 0, 0],
                        port: 0,
                    }),
                    out,
                ),
            )
        }
    }
}

fn query_scce(config: &ModelConfig, prompt: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut augmented = [0u8; AUGMENTED_PROMPT_CAP];
    let augmented_len = build_augmented_prompt(prompt, config, &mut augmented);

    let mut body = [0u8; REQUEST_BODY_CAP];
    let mut body_len = 0usize;
    append_bytes(&mut body, &mut body_len, b"{\"message\":");
    append_json_string(&mut body, &mut body_len, &augmented[..augmented_len]);
    append_bytes(&mut body, &mut body_len, b",\"attachments\":[]}");

    let mut http_body = [0u8; HTTP_RESPONSE_CAP];
    let result = http_post_json(
        config.scce_host,
        config.scce_port,
        b"/api/chat",
        &body[..body_len],
        &mut http_body,
    )?;

    if result.status != 200 {
        return Some(compose_http_error(
            b"SCCE",
            result.status,
            &http_body[..result.body_len],
            out,
        ));
    }

    let mut extracted = [0u8; RESPONSE_CAP];
    let message_len =
        json_extract_string(&http_body[..result.body_len], b"message", &mut extracted);
    if message_len > 0 {
        return Some(copy_slice(out, &extracted[..message_len]));
    }

    let error_len = json_extract_string(&http_body[..result.body_len], b"error", &mut extracted);
    if error_len > 0 {
        let mut len = 0usize;
        append_bytes(out, &mut len, b"SCCE reported an error: ");
        append_bytes(out, &mut len, &extracted[..error_len]);
        return Some(len);
    }

    Some(copy_slice(
        out,
        b"SCCE replied, but the payload did not contain a readable message field.",
    ))
}

fn query_ollama(
    config: &ModelConfig,
    requested_model: &[u8],
    prompt: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let ollama_model = if requested_model.is_empty() {
        config.ollama_model()
    } else {
        requested_model
    };

    let mut augmented = [0u8; AUGMENTED_PROMPT_CAP];
    let augmented_len = build_augmented_prompt(prompt, config, &mut augmented);

    let mut body = [0u8; REQUEST_BODY_CAP];
    let mut body_len = 0usize;
    append_bytes(&mut body, &mut body_len, b"{\"model\":");
    append_json_string(&mut body, &mut body_len, ollama_model);
    append_bytes(&mut body, &mut body_len, b",\"prompt\":");
    append_json_string(&mut body, &mut body_len, &augmented[..augmented_len]);
    append_bytes(&mut body, &mut body_len, b",\"stream\":false}");

    let mut http_body = [0u8; HTTP_RESPONSE_CAP];
    let result = http_post_json(
        config.ollama_host,
        config.ollama_port,
        b"/api/generate",
        &body[..body_len],
        &mut http_body,
    )?;

    if result.status != 200 {
        return Some(compose_http_error(
            b"Ollama",
            result.status,
            &http_body[..result.body_len],
            out,
        ));
    }

    let mut extracted = [0u8; RESPONSE_CAP];
    let response_len =
        json_extract_string(&http_body[..result.body_len], b"response", &mut extracted);
    if response_len > 0 {
        return Some(copy_slice(out, &extracted[..response_len]));
    }

    let error_len = json_extract_string(&http_body[..result.body_len], b"error", &mut extracted);
    if error_len > 0 {
        let mut len = 0usize;
        append_bytes(out, &mut len, b"Ollama reported an error: ");
        append_bytes(out, &mut len, &extracted[..error_len]);
        return Some(len);
    }

    Some(copy_slice(
        out,
        b"Ollama replied, but the payload did not contain a readable response field.",
    ))
}

fn compose_builtin_reply(
    prompt: &[u8],
    config: &ModelConfig,
    failure: Option<BackendFailure>,
    out: &mut [u8],
) -> usize {
    let prompt = trim_ascii(prompt);
    let mut len = 0usize;
    append_bytes(out, &mut len, b"GraphOS builtin operator lane is active.\n");

    if let Some(failure) = failure {
        if failure.backend == Backend::Auto {
            append_bytes(
                out,
                &mut len,
                b"Auto routing could not reach SCCE or Ollama, so builtin synthesis took over.\n",
            );
        } else {
            append_bytes(out, &mut len, b"Requested backend unreachable: ");
            append_bytes(out, &mut len, backend_label(failure.backend));
            append_bytes(out, &mut len, b" @ ");
            append_ipv4(out, &mut len, failure.host);
            append_byte(out, &mut len, b':');
            append_u16(out, &mut len, failure.port);
            append_byte(out, &mut len, b'\n');
        }
    }

    append_bytes(out, &mut len, b"Configured routes: backend=");
    append_bytes(out, &mut len, backend_label(config.backend));
    append_bytes(out, &mut len, b" scce=");
    append_ipv4(out, &mut len, config.scce_host);
    append_byte(out, &mut len, b':');
    append_u16(out, &mut len, config.scce_port);
    append_bytes(out, &mut len, b" ollama=");
    append_ipv4(out, &mut len, config.ollama_host);
    append_byte(out, &mut len, b':');
    append_u16(out, &mut len, config.ollama_port);
    append_bytes(out, &mut len, b" model=");
    append_bytes(out, &mut len, config.ollama_model());
    append_byte(out, &mut len, b'\n');

    let mut wrote_guidance = false;
    if prompt.is_empty() {
        append_bytes(
            out,
            &mut len,
            b"Prompt was empty. Ask about system state, SCCE, PowerWalk, self-healing, or point me at a concrete task.\n",
        );
        wrote_guidance = true;
    }

    if contains_ascii(prompt, b"status")
        || contains_ascii(prompt, b"backend")
        || contains_ascii(prompt, b"health")
        || contains_ascii(prompt, b"ready")
    {
        append_bytes(
            out,
            &mut len,
            b"Status readout: modeld can now route to SCCE for grounded synthesis, Ollama for optional local LLM generation, or builtin doctrine mode when the host services are absent.\n",
        );
        wrote_guidance = true;
    }

    if contains_ascii(prompt, b"scce")
        || contains_ascii(prompt, b"ai")
        || contains_ascii(prompt, b"llm")
        || contains_ascii(prompt, b"model")
    {
        append_bytes(
            out,
            &mut len,
            b"SCCE is the graph-first evidence and synthesis lane. Ollama is an optional local power-up, not the control plane. Graph context and doctrine stay in front either way.\n",
        );
        wrote_guidance = true;
    }

    if contains_ascii(prompt, b"wtg")
        || contains_ascii(prompt, b"powerwalk")
        || contains_ascii(prompt, b"castlehale")
        || contains_ascii(prompt, b"predict")
        || contains_ascii(prompt, b"classifier")
        || contains_ascii(prompt, b"walsh")
    {
        append_bytes(
            out,
            &mut len,
            b"WTG / PowerWalk is treated here as the predictive crown-jewel lane. Walsh-Hadamard is explicitly kept separate from the Walsh Technical Group predictive classifier math.\n",
        );
        append_doctrine(config, out, &mut len);
        wrote_guidance = true;
    }

    if contains_ascii(prompt, b"self-heal")
        || contains_ascii(prompt, b"heal")
        || contains_ascii(prompt, b"recover")
        || contains_ascii(prompt, b"restart")
    {
        append_bytes(
            out,
            &mut len,
            b"Self-healing in this image means graph-observed context, predictive hints, bounded service recovery, and human-visible provenance. It is an operator loop, not blind mutation.\n",
        );
        wrote_guidance = true;
    }

    if !wrote_guidance {
        append_bytes(
            out,
            &mut len,
            b"Builtin synthesis is doctrine-first and runtime-aware. Bring SCCE online for the full graph-native answer engine, or Ollama online for local model range when you want more generative depth.\n",
        );
    }

    append_runtime_snapshot(out, &mut len);
    len
}

fn compose_status_reply(route: Route<'_>, config: &ModelConfig, out: &mut [u8]) -> usize {
    let mut len = 0usize;
    append_bytes(out, &mut len, b"GraphOS modeld rollout status.\n");
    append_bytes(out, &mut len, b"Configured backend=");
    append_bytes(out, &mut len, backend_label(config.backend));
    append_byte(out, &mut len, b'\n');
    append_bytes(out, &mut len, b"Selected route=");
    append_bytes(out, &mut len, backend_label(route.backend));
    append_byte(out, &mut len, b'\n');
    append_bytes(out, &mut len, b"SCCE host=");
    append_ipv4(out, &mut len, config.scce_host);
    append_byte(out, &mut len, b':');
    append_u16(out, &mut len, config.scce_port);
    append_byte(out, &mut len, b'\n');
    append_bytes(out, &mut len, b"Ollama host=");
    append_ipv4(out, &mut len, config.ollama_host);
    append_byte(out, &mut len, b':');
    append_u16(out, &mut len, config.ollama_port);
    append_bytes(out, &mut len, b" model=");
    append_bytes(out, &mut len, route.ollama_model);
    append_byte(out, &mut len, b'\n');
    append_bytes(
        out,
        &mut len,
        b"Guest commands: /ai status, /ai help. Model overrides: scce | builtin | ollama[:model].\n",
    );
    append_runtime_snapshot(out, &mut len);
    len
}

fn compose_help_reply(config: &ModelConfig, out: &mut [u8]) -> usize {
    let mut len = 0usize;
    append_bytes(out, &mut len, b"GraphOS modeld command help.\n");
    append_bytes(
        out,
        &mut len,
        b"/ai status -> local rollout status without leaving the guest.\n",
    );
    append_bytes(
        out,
        &mut len,
        b"/ai help -> this help text.\n",
    );
    append_bytes(
        out,
        &mut len,
        b"Model hints: scce | builtin | ollama[:model]. The packaged default is now SCCE-first.\n",
    );
    append_bytes(out, &mut len, b"Configured backend=");
    append_bytes(out, &mut len, backend_label(config.backend));
    append_byte(out, &mut len, b'\n');
    append_runtime_snapshot(out, &mut len);
    len
}

fn compose_http_error(label: &[u8], status: u16, body: &[u8], out: &mut [u8]) -> usize {
    let mut len = 0usize;
    append_bytes(out, &mut len, label);
    append_bytes(out, &mut len, b" HTTP ");
    append_u16(out, &mut len, status);

    let mut extracted = [0u8; 256];
    let error_len = json_extract_string(body, b"error", &mut extracted);
    if error_len > 0 {
        append_bytes(out, &mut len, b": ");
        append_bytes(out, &mut len, &extracted[..error_len]);
        return len;
    }

    let message_len = json_extract_string(body, b"message", &mut extracted);
    if message_len > 0 {
        append_bytes(out, &mut len, b": ");
        append_bytes(out, &mut len, &extracted[..message_len]);
        return len;
    }

    append_bytes(
        out,
        &mut len,
        b" while talking to the host inference service.",
    );
    len
}

fn build_augmented_prompt(user_prompt: &[u8], config: &ModelConfig, out: &mut [u8]) -> usize {
    let mut len = 0usize;
    append_bytes(
        out,
        &mut len,
        b"You are GraphOS modeld. Answer as a graph-first operating participant, not a generic chatbot.\n",
    );
    append_bytes(
        out,
        &mut len,
        b"Priorities: grounded synthesis, explicit uncertainty, and respect for the Walsh Technical Group doctrine.\n",
    );
    append_doctrine(config, out, &mut len);
    append_runtime_snapshot(out, &mut len);
    append_bytes(out, &mut len, b"user_request:\n");
    append_bytes(out, &mut len, trim_ascii(user_prompt));
    len
}

fn append_runtime_snapshot(out: &mut [u8], len: &mut usize) {
    append_bytes(out, len, b"runtime:\n");

    if let Some(ctx) = workspace_context::read() {
        append_bytes(out, len, b"  scope=");
        append_bytes(out, len, ctx.scope());
        append_byte(out, len, b'\n');
        append_bytes(out, len, b"  focus=");
        append_bytes(out, len, ctx.focus());
        append_byte(out, len, b'\n');
        append_bytes(out, len, b"  source=");
        append_bytes(out, len, ctx.source());
        append_byte(out, len, b' ');
        append_bytes(out, len, if ctx.is_dir { b"[dir]" } else { b"[file]" });
        append_byte(out, len, b'\n');
    } else {
        append_bytes(out, len, b"  focus=/graph\n");
    }

    append_bytes(out, len, b"  services=");
    let mut first = true;
    let mut idx = 0usize;
    while idx < SERVICE_NAMES.len() {
        if runtime::registry_lookup(SERVICE_NAMES[idx]).is_some() {
            if !first {
                append_byte(out, len, b',');
            }
            append_bytes(out, len, SERVICE_NAMES[idx]);
            first = false;
        }
        idx += 1;
    }
    if first {
        append_bytes(out, len, b"none");
    }
    append_byte(out, len, b'\n');

    if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
        append_bytes(out, len, b"  graph_em_transitions=");
        append_u32(out, len, transitions);
        append_bytes(out, len, b" epoch=");
        append_u32(out, len, epoch);
        append_byte(out, len, b'\n');
    }
}

fn append_doctrine(config: &ModelConfig, out: &mut [u8], len: &mut usize) {
    append_bytes(out, len, b"doctrine:\n");
    append_bytes(out, len, trim_ascii(config.doctrine()));
    append_byte(out, len, b'\n');
}

fn load_config() -> ModelConfig {
    let mut config = ModelConfig::default();
    let mut raw = [0u8; CONFIG_CAP];
    let mut idx = 0usize;
    while idx < CONFIG_PATHS.len() {
        let len = read_small_file(CONFIG_PATHS[idx], &mut raw);
        if len > 0 {
            parse_config(&mut config, &raw[..len]);
            return config;
        }
        idx += 1;
    }
    config
}

fn parse_config(config: &mut ModelConfig, raw: &[u8]) {
    let mut field = [0u8; 64];

    let backend_len = json_extract_string(raw, b"backend", &mut field);
    if backend_len > 0 {
        config.backend = parse_backend(&field[..backend_len]);
    }

    let scce_host_len = json_extract_string(raw, b"scce_host", &mut field);
    if scce_host_len > 0 {
        if let Some(host) = parse_ipv4(&field[..scce_host_len]) {
            config.scce_host = host;
        }
    }

    if let Some(port) = json_extract_u16(raw, b"scce_port") {
        config.scce_port = port;
    }

    let ollama_host_len = json_extract_string(raw, b"ollama_host", &mut field);
    if ollama_host_len > 0 {
        if let Some(host) = parse_ipv4(&field[..ollama_host_len]) {
            config.ollama_host = host;
        }
    }

    if let Some(port) = json_extract_u16(raw, b"ollama_port") {
        config.ollama_port = port;
    }

    let model_len = json_extract_string(raw, b"ollama_model", &mut config.ollama_model);
    if model_len > 0 {
        config.ollama_model_len = model_len;
    }

    let doctrine_len = json_extract_string(raw, b"doctrine", &mut config.doctrine);
    if doctrine_len > 0 {
        config.doctrine_len = doctrine_len;
    }
}

fn read_small_file(path: &[u8], out: &mut [u8]) -> usize {
    let fd = runtime::vfs_open(path);
    if fd == u64::MAX {
        return 0;
    }

    let raw = runtime::vfs_read(fd, out);
    let _ = runtime::vfs_close(fd);
    if raw == u64::MAX {
        0
    } else {
        (raw as usize).min(out.len())
    }
}

fn http_post_json(
    host: [u8; 4],
    port: u16,
    path: &[u8],
    json_body: &[u8],
    out: &mut [u8],
) -> Option<HttpResult> {
    let socket = runtime::socket_open()?;
    if !runtime::socket_connect(&socket, u32::from_be_bytes(host), port) {
        let _ = runtime::socket_close(&socket);
        return None;
    }

    let mut req = [0u8; REQUEST_BODY_CAP + 320];
    let mut req_len = 0usize;
    append_bytes(&mut req, &mut req_len, b"POST ");
    append_bytes(&mut req, &mut req_len, path);
    append_bytes(&mut req, &mut req_len, b" HTTP/1.1\r\nHost: ");
    append_ipv4(&mut req, &mut req_len, host);
    append_bytes(
        &mut req,
        &mut req_len,
        b"\r\nContent-Type: application/json\r\nContent-Length: ",
    );
    append_u32(&mut req, &mut req_len, json_body.len() as u32);
    append_bytes(&mut req, &mut req_len, b"\r\nConnection: close\r\n\r\n");
    append_bytes(&mut req, &mut req_len, json_body);

    if runtime::socket_send(&socket, &req[..req_len]).is_none() {
        let _ = runtime::socket_close(&socket);
        return None;
    }

    let mut http = [0u8; HTTP_RESPONSE_CAP];
    let recv_len = runtime::socket_recv_all(&socket, &mut http);
    let _ = runtime::socket_close(&socket);
    if recv_len < 12 {
        return Some(HttpResult {
            status: 0,
            body_len: 0,
        });
    }

    let status = parse_http_status(&http[..recv_len]);
    let body_start = find_http_body_start(&http[..recv_len]);
    if body_start >= recv_len {
        return Some(HttpResult {
            status,
            body_len: 0,
        });
    }

    let body_len = (recv_len - body_start).min(out.len());
    out[..body_len].copy_from_slice(&http[body_start..body_start + body_len]);
    Some(HttpResult { status, body_len })
}

fn parse_http_status(http: &[u8]) -> u16 {
    if http.len() < 12 || !http[..8].starts_with(b"HTTP/1.") {
        return 0;
    }

    let mut value = 0u16;
    let mut idx = 9usize;
    while idx < http.len() && idx < 12 && http[idx].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((http[idx] - b'0') as u16);
        idx += 1;
    }
    value
}

fn find_http_body_start(http: &[u8]) -> usize {
    let mut idx = 0usize;
    while idx + 3 < http.len() {
        if &http[idx..idx + 4] == b"\r\n\r\n" {
            return idx + 4;
        }
        idx += 1;
    }
    http.len()
}

fn parse_chat_request(payload: &[u8]) -> Option<ChatRequest<'_>> {
    let rest = payload.strip_prefix(b"chat|")?;
    let (reply_raw, rest) = split_once(rest, b'|')?;
    let reply_channel = parse_u32(reply_raw)?;
    let (model, prompt) = split_once(rest, b'|')?;
    Some(ChatRequest {
        reply_channel,
        model,
        prompt,
    })
}

fn select_route<'a>(config: &'a ModelConfig, requested_model: &'a [u8]) -> Route<'a> {
    let requested_model = trim_ascii(requested_model);
    if requested_model.is_empty() {
        return Route {
            backend: config.backend,
            ollama_model: config.ollama_model(),
        };
    }

    if eq_ignore_ascii_case(requested_model, b"builtin") {
        return Route {
            backend: Backend::Builtin,
            ollama_model: config.ollama_model(),
        };
    }
    if eq_ignore_ascii_case(requested_model, b"scce") {
        return Route {
            backend: Backend::Scce,
            ollama_model: config.ollama_model(),
        };
    }
    if eq_ignore_ascii_case(requested_model, b"ollama") {
        return Route {
            backend: Backend::Ollama,
            ollama_model: config.ollama_model(),
        };
    }
    if let Some(model) = split_backend_hint(requested_model, b"ollama") {
        return Route {
            backend: Backend::Ollama,
            ollama_model: if model.is_empty() {
                config.ollama_model()
            } else {
                model
            },
        };
    }
    if split_backend_hint(requested_model, b"scce").is_some() {
        return Route {
            backend: Backend::Scce,
            ollama_model: config.ollama_model(),
        };
    }
    if split_backend_hint(requested_model, b"builtin").is_some() {
        return Route {
            backend: Backend::Builtin,
            ollama_model: config.ollama_model(),
        };
    }

    if config.backend == Backend::Ollama {
        Route {
            backend: Backend::Ollama,
            ollama_model: requested_model,
        }
    } else {
        Route {
            backend: config.backend,
            ollama_model: config.ollama_model(),
        }
    }
}

fn split_backend_hint<'a>(value: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if value.len() <= prefix.len() || value[prefix.len()] != b':' {
        return None;
    }
    if !eq_ignore_ascii_case(&value[..prefix.len()], prefix) {
        return None;
    }
    Some(trim_ascii(&value[prefix.len() + 1..]))
}

fn parse_backend(value: &[u8]) -> Backend {
    if eq_ignore_ascii_case(value, b"builtin") {
        Backend::Builtin
    } else if eq_ignore_ascii_case(value, b"scce") {
        Backend::Scce
    } else if eq_ignore_ascii_case(value, b"ollama") {
        Backend::Ollama
    } else {
        Backend::Auto
    }
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if !b.is_ascii_digit() {
            return None;
        }
        value = value.saturating_mul(10).saturating_add((b - b'0') as u32);
        idx += 1;
    }
    Some(value)
}

fn parse_u16(bytes: &[u8]) -> Option<u16> {
    let value = parse_u32(bytes)?;
    if value > u16::MAX as u32 {
        None
    } else {
        Some(value as u16)
    }
}

fn parse_ipv4(bytes: &[u8]) -> Option<[u8; 4]> {
    let bytes = trim_ascii(bytes);
    if eq_ignore_ascii_case(bytes, b"localhost") {
        return Some([127, 0, 0, 1]);
    }

    let mut out = [0u8; 4];
    let mut part = 0usize;
    let mut value = 0u16;
    let mut saw_digit = false;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if b == b'.' {
            if !saw_digit || part >= 4 || value > 255 {
                return None;
            }
            out[part] = value as u8;
            part += 1;
            value = 0;
            saw_digit = false;
        } else if b.is_ascii_digit() {
            saw_digit = true;
            value = value.saturating_mul(10).saturating_add((b - b'0') as u16);
        } else {
            return None;
        }
        idx += 1;
    }
    if !saw_digit || part != 3 || value > 255 {
        return None;
    }
    out[3] = value as u8;
    Some(out)
}

fn json_extract_string(body: &[u8], key: &[u8], out: &mut [u8]) -> usize {
    let mut pattern = [0u8; 64];
    if key.len() + 2 > pattern.len() {
        return 0;
    }
    pattern[0] = b'"';
    pattern[1..1 + key.len()].copy_from_slice(key);
    pattern[key.len() + 1] = b'"';
    let pattern = &pattern[..key.len() + 2];
    let Some(mut idx) = find_subslice(body, pattern) else {
        return 0;
    };

    idx += pattern.len();
    idx = skip_ascii_ws(body, idx);
    if idx >= body.len() || body[idx] != b':' {
        return 0;
    }
    idx += 1;
    idx = skip_ascii_ws(body, idx);
    if idx >= body.len() || body[idx] != b'"' {
        return 0;
    }
    idx += 1;

    let mut out_len = 0usize;
    while idx < body.len() && out_len < out.len() {
        let b = body[idx];
        if b == b'"' {
            return out_len;
        }
        if b == b'\\' {
            idx += 1;
            if idx >= body.len() {
                break;
            }
            let esc = body[idx];
            match esc {
                b'"' | b'\\' | b'/' => {
                    out[out_len] = esc;
                    out_len += 1;
                }
                b'b' => {
                    out[out_len] = 8;
                    out_len += 1;
                }
                b'f' => {
                    out[out_len] = 12;
                    out_len += 1;
                }
                b'n' => {
                    out[out_len] = b'\n';
                    out_len += 1;
                }
                b'r' => {
                    out[out_len] = b'\r';
                    out_len += 1;
                }
                b't' => {
                    out[out_len] = b'\t';
                    out_len += 1;
                }
                b'u' => {
                    out[out_len] = b'?';
                    out_len += 1;
                    let mut skip = 0usize;
                    while skip < 4 && idx + 1 < body.len() {
                        idx += 1;
                        skip += 1;
                    }
                }
                _ => {}
            }
        } else {
            out[out_len] = b;
            out_len += 1;
        }
        idx += 1;
    }
    out_len
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let mut idx = 0usize;
    while idx + needle.len() <= haystack.len() {
        if &haystack[idx..idx + needle.len()] == needle {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

fn skip_ascii_ws(bytes: &[u8], mut idx: usize) -> usize {
    while idx < bytes.len() {
        match bytes[idx] {
            b' ' | b'\t' | b'\r' | b'\n' => idx += 1,
            _ => break,
        }
    }
    idx
}

fn contains_ascii(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let mut idx = 0usize;
    while idx + needle.len() <= haystack.len() {
        if eq_ignore_ascii_case(&haystack[idx..idx + needle.len()], needle) {
            return true;
        }
        idx += 1;
    }
    false
}

fn json_extract_u16(body: &[u8], key: &[u8]) -> Option<u16> {
    let mut pattern = [0u8; 64];
    if key.len() + 2 > pattern.len() {
        return None;
    }
    pattern[0] = b'"';
    pattern[1..1 + key.len()].copy_from_slice(key);
    pattern[key.len() + 1] = b'"';
    let pattern = &pattern[..key.len() + 2];
    let mut idx = find_subslice(body, pattern)?;
    idx += pattern.len();
    idx = skip_ascii_ws(body, idx);
    if idx >= body.len() || body[idx] != b':' {
        return None;
    }
    idx += 1;
    idx = skip_ascii_ws(body, idx);
    if idx >= body.len() {
        return None;
    }
    if body[idx] == b'"' {
        let mut field = [0u8; 8];
        let len = json_extract_string(body, key, &mut field);
        return parse_u16(&field[..len]);
    }

    let start = idx;
    while idx < body.len() && body[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == start {
        return None;
    }
    parse_u16(&body[start..idx])
}

fn split_once(bytes: &[u8], sep: u8) -> Option<(&[u8], &[u8])> {
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == sep {
            return Some((&bytes[..idx], &bytes[idx + 1..]));
        }
        idx += 1;
    }
    None
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

fn eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut idx = 0usize;
    while idx < left.len() {
        if left[idx].to_ascii_lowercase() != right[idx].to_ascii_lowercase() {
            return false;
        }
        idx += 1;
    }
    true
}

fn stream_reply(reply_channel: u32, label: &[u8], body: &[u8]) {
    let _ = runtime::channel_send(reply_channel, b"modeld[", TAG_INFERENCE_RESPONSE);
    let _ = runtime::channel_send(reply_channel, label, TAG_INFERENCE_RESPONSE);
    let _ = runtime::channel_send(reply_channel, b"]: ", TAG_INFERENCE_RESPONSE);

    let mut offset = 0usize;
    while offset < body.len() {
        let end = core::cmp::min(offset + STREAM_CHUNK, body.len());
        let _ = runtime::channel_send(reply_channel, &body[offset..end], TAG_INFERENCE_RESPONSE);
        offset = end;
    }

    let _ = runtime::channel_send(reply_channel, b"[[done]]", TAG_INFERENCE_RESPONSE);
}

fn backend_label(backend: Backend) -> &'static [u8] {
    match backend {
        Backend::Auto => b"auto",
        Backend::Builtin => b"builtin",
        Backend::Scce => b"scce",
        Backend::Ollama => b"ollama",
    }
}

fn copy_slice(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());
    dst[..len].copy_from_slice(&src[..len]);
    len
}

fn copy_field<const N: usize>(dst: &mut [u8; N], src: &[u8]) -> usize {
    let len = src.len().min(N);
    dst[..len].copy_from_slice(&src[..len]);
    len
}

fn append_json_string(out: &mut [u8], len: &mut usize, src: &[u8]) {
    append_byte(out, len, b'"');
    let mut idx = 0usize;
    while idx < src.len() && *len < out.len() {
        match src[idx] {
            b'\\' => append_bytes(out, len, b"\\\\"),
            b'"' => append_bytes(out, len, b"\\\""),
            b'\n' => append_bytes(out, len, b"\\n"),
            b'\r' => append_bytes(out, len, b"\\r"),
            b'\t' => append_bytes(out, len, b"\\t"),
            b if (0x20..=0x7E).contains(&b) => append_byte(out, len, b),
            _ => append_byte(out, len, b'?'),
        }
        idx += 1;
    }
    append_byte(out, len, b'"');
}

fn append_ipv4(out: &mut [u8], len: &mut usize, ip: [u8; 4]) {
    append_u32(out, len, ip[0] as u32);
    append_byte(out, len, b'.');
    append_u32(out, len, ip[1] as u32);
    append_byte(out, len, b'.');
    append_u32(out, len, ip[2] as u32);
    append_byte(out, len, b'.');
    append_u32(out, len, ip[3] as u32);
}

fn append_u16(out: &mut [u8], len: &mut usize, value: u16) {
    append_u32(out, len, value as u32);
}

fn append_u32(out: &mut [u8], len: &mut usize, mut value: u32) {
    if value == 0 {
        append_byte(out, len, b'0');
        return;
    }

    let mut digits = [0u8; 10];
    let mut digits_len = 0usize;
    while value > 0 {
        digits[digits_len] = b'0' + (value % 10) as u8;
        digits_len += 1;
        value /= 10;
    }
    while digits_len > 0 {
        digits_len -= 1;
        append_byte(out, len, digits[digits_len]);
    }
}

fn append_bytes(out: &mut [u8], len: &mut usize, src: &[u8]) {
    if *len >= out.len() {
        return;
    }
    let available = out.len() - *len;
    let copy = src.len().min(available);
    out[*len..*len + copy].copy_from_slice(&src[..copy]);
    *len += copy;
}

fn append_byte(out: &mut [u8], len: &mut usize, byte: u8) {
    if *len < out.len() {
        out[*len] = byte;
        *len += 1;
    }
}
