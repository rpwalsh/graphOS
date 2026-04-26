// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! terminal - GraphOS built-in developer shell.
//!
//! Modern ring-3 terminal with:
//! - GraphOS shell chrome and quick-launch actions
//! - command history with arrow-key recall
//! - orchestrator-paced cursor blink and time reporting
//! - graph-first service, registry, and tooling commands

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;
#[path = "../workspace_context.rs"]
mod workspace_context;

use core::panic::PanicInfo;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::{
    geom::Rect,
    tokens::{Theme, tokens},
    widgets::{
        ButtonKind, draw_button, draw_command_bar, draw_panel, draw_scroll_track, draw_stat_card,
        draw_window_frame,
    },
};

const WIN_W: u32 = 920;
const WIN_H: u32 = 560;
const THEME: Theme = Theme::DarkGlass;

const HEADER_H: u32 = 32;
const COMMAND_H: u32 = 28;
const METRICS_H: u32 = 54;
const SIDEPANEL_W: u32 = 244;
const LINE_H: u32 = 12;
const OUTPUT_LINES: usize = 96;
const LINE_W: usize = 160;
const CMD_MAX: usize = 128;
const HISTORY_MAX: usize = 16;
const ACTION_COUNT: usize = 8;
const MAX_PATH: usize = 64;
const OUTPUT_VIEW_H: u32 = WIN_H - HEADER_H - COMMAND_H - METRICS_H - 28;
const LINES_VISIBLE: usize = (OUTPUT_VIEW_H / LINE_H) as usize;
const CORE_SERVICE_COUNT: usize = 8;
const CORE_SERVICE_NAMES: [&[u8]; CORE_SERVICE_COUNT] = [
    b"init",
    b"servicemgr",
    b"graphd",
    b"modeld",
    b"trainerd",
    b"artifactsd",
    b"sysd",
    b"compositor",
];

const ACTION_LABELS: [&[u8]; ACTION_COUNT] = [
    b"Files",
    b"Editor",
    b"Notes",
    b"Paint",
    b"Calc",
    b"Settings",
    b"AI",
    b"SSH",
];

#[derive(Clone, Copy)]
struct Path {
    data: [u8; MAX_PATH],
    len: usize,
}

impl Path {
    const fn root() -> Self {
        let mut data = [0u8; MAX_PATH];
        data[0] = b'/';
        Self { data, len: 1 }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut path = Self::root();
        if bytes.is_empty() {
            return path;
        }
        let len = bytes.len().min(MAX_PATH);
        path.data[..len].copy_from_slice(&bytes[..len]);
        path.len = len;
        if path.data[0] != b'/' {
            path.data[0] = b'/';
            path.len = 1;
        }
        path
    }

    fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    fn is_root(&self) -> bool {
        self.len <= 1
    }

    fn join(&self, name: &[u8]) -> Self {
        let mut next = *self;
        if next.len == 0 {
            next.data[0] = b'/';
            next.len = 1;
        }
        if next.data[next.len - 1] != b'/' && next.len < MAX_PATH {
            next.data[next.len] = b'/';
            next.len += 1;
        }
        let rem = MAX_PATH.saturating_sub(next.len);
        let take = rem.min(name.len());
        if take > 0 {
            next.data[next.len..next.len + take].copy_from_slice(&name[..take]);
            next.len += take;
        }
        next
    }

    fn parent(&self) -> Self {
        if self.is_root() {
            return Self::root();
        }
        let mut end = self.len.saturating_sub(1);
        while end > 1 && self.data[end] != b'/' {
            end -= 1;
        }
        Self::from_bytes(&self.data[..end])
    }
}

struct Terminal {
    lines: [[u8; LINE_W]; OUTPUT_LINES],
    line_lens: [u16; OUTPUT_LINES],
    head: usize,
    count: usize,
    scroll: usize,
    cmd: [u8; CMD_MAX],
    cmd_len: usize,
    history: [[u8; CMD_MAX]; HISTORY_MAX],
    history_lens: [u8; HISTORY_MAX],
    history_count: usize,
    history_cursor: Option<usize>,
    history_draft: [u8; CMD_MAX],
    history_draft_len: usize,
    cwd: Path,
    now_ms: u64,
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    status: [u8; 64],
    status_len: usize,
    context_scope: [u8; workspace_context::PATH_CAP],
    context_scope_len: usize,
    context_focus: [u8; workspace_context::PATH_CAP],
    context_focus_len: usize,
    context_source: [u8; workspace_context::SOURCE_CAP],
    context_source_len: usize,
    context_is_dir: bool,
    last_context_refresh_ms: u64,
}

impl Terminal {
    fn new() -> Self {
        let mut term = Self {
            lines: [[0u8; LINE_W]; OUTPUT_LINES],
            line_lens: [0u16; OUTPUT_LINES],
            head: 0,
            count: 0,
            scroll: 0,
            cmd: [0u8; CMD_MAX],
            cmd_len: 0,
            history: [[0u8; CMD_MAX]; HISTORY_MAX],
            history_lens: [0u8; HISTORY_MAX],
            history_count: 0,
            history_cursor: None,
            history_draft: [0u8; CMD_MAX],
            history_draft_len: 0,
            cwd: Path::root(),
            now_ms: 0,
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            status: [0u8; 64],
            status_len: 0,
            context_scope: [0u8; workspace_context::PATH_CAP],
            context_scope_len: 0,
            context_focus: [0u8; workspace_context::PATH_CAP],
            context_focus_len: 0,
            context_source: [0u8; workspace_context::SOURCE_CAP],
            context_source_len: 0,
            context_is_dir: true,
            last_context_refresh_ms: 0,
        };
        term.cwd = Path::from_bytes(b"/graph");
        term.refresh_workspace_context(true);
        term.push_line(b"GraphOS Terminal ready");
        term.push_line(b"Type 'help' for graph-first shell commands.");
        term.set_status(b"Graph shell online.");
        term
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn context_scope(&self) -> &[u8] {
        if self.context_scope_len > 0 {
            &self.context_scope[..self.context_scope_len]
        } else {
            b"/graph"
        }
    }

    fn context_focus(&self) -> &[u8] {
        if self.context_focus_len > 0 {
            &self.context_focus[..self.context_focus_len]
        } else {
            self.context_scope()
        }
    }

    fn context_source(&self) -> &[u8] {
        if self.context_source_len > 0 {
            &self.context_source[..self.context_source_len]
        } else {
            b"shell"
        }
    }

    fn context_dir_path(&self) -> Path {
        if self.context_is_dir {
            Path::from_bytes(self.context_focus())
        } else {
            Path::from_bytes(workspace_context::parent_path(self.context_focus()))
        }
    }

    fn refresh_workspace_context(&mut self, adopt_cwd: bool) -> bool {
        let Some(ctx) = workspace_context::read() else {
            return false;
        };

        let mut changed = false;
        changed |= replace_bytes(
            &mut self.context_scope,
            &mut self.context_scope_len,
            ctx.scope(),
        );
        changed |= replace_bytes(
            &mut self.context_focus,
            &mut self.context_focus_len,
            ctx.focus(),
        );
        changed |= replace_bytes(
            &mut self.context_source,
            &mut self.context_source_len,
            ctx.source(),
        );

        if self.context_is_dir != ctx.is_dir {
            self.context_is_dir = ctx.is_dir;
            changed = true;
        }

        if adopt_cwd {
            self.cwd = self.context_dir_path();
        }

        changed
    }

    fn adopt_context_directory(&mut self) {
        self.cwd = self.context_dir_path();
        let mut cwd = [0u8; MAX_PATH];
        let cwd_bytes = self.cwd.as_bytes();
        let cwd_len = cwd_bytes.len().min(cwd.len());
        cwd[..cwd_len].copy_from_slice(&cwd_bytes[..cwd_len]);
        self.push_line(&cwd[..cwd_len]);
        self.sync_workspace_context();
        self.set_status(b"Shell moved to graph focus.");
    }

    fn sync_workspace_context(&self) {
        let _ =
            workspace_context::write(self.cwd.as_bytes(), self.cwd.as_bytes(), b"terminal", true);
    }

    fn push_line(&mut self, text: &[u8]) {
        let idx = (self.head + self.count) % OUTPUT_LINES;
        let copy = text.len().min(LINE_W);
        self.lines[idx] = [0u8; LINE_W];
        self.lines[idx][..copy].copy_from_slice(&text[..copy]);
        self.line_lens[idx] = copy as u16;
        if self.count < OUTPUT_LINES {
            self.count += 1;
        } else {
            self.head = (self.head + 1) % OUTPUT_LINES;
        }
        self.scroll = self.count.saturating_sub(LINES_VISIBLE);
    }

    fn line(&self, abs_idx: usize) -> &[u8] {
        let idx = (self.head + abs_idx) % OUTPUT_LINES;
        let len = self.line_lens[idx] as usize;
        &self.lines[idx][..len]
    }

    fn save_history(&mut self, cmd: &[u8]) {
        if cmd.is_empty() {
            return;
        }
        if self.history_count > 0 {
            let last = self.history_count - 1;
            let len = self.history_lens[last] as usize;
            if &self.history[last][..len] == cmd {
                return;
            }
        }
        if self.history_count < HISTORY_MAX {
            let slot = self.history_count;
            self.history[slot] = [0u8; CMD_MAX];
            self.history[slot][..cmd.len()].copy_from_slice(cmd);
            self.history_lens[slot] = cmd.len() as u8;
            self.history_count += 1;
        } else {
            let mut i = 1usize;
            while i < HISTORY_MAX {
                self.history[i - 1] = self.history[i];
                self.history_lens[i - 1] = self.history_lens[i];
                i += 1;
            }
            self.history[HISTORY_MAX - 1] = [0u8; CMD_MAX];
            self.history[HISTORY_MAX - 1][..cmd.len()].copy_from_slice(cmd);
            self.history_lens[HISTORY_MAX - 1] = cmd.len() as u8;
        }
        self.history_cursor = None;
    }

    fn copy_into_cmd(&mut self, src: &[u8]) {
        self.cmd = [0u8; CMD_MAX];
        let len = src.len().min(CMD_MAX);
        self.cmd[..len].copy_from_slice(&src[..len]);
        self.cmd_len = len;
    }

    fn history_prev(&mut self) {
        if self.history_count == 0 {
            return;
        }
        let next = match self.history_cursor {
            None => {
                self.history_draft[..self.cmd_len].copy_from_slice(&self.cmd[..self.cmd_len]);
                self.history_draft_len = self.cmd_len;
                self.history_count - 1
            }
            Some(0) => 0,
            Some(idx) => idx - 1,
        };
        let len = self.history_lens[next] as usize;
        let mut buf = [0u8; CMD_MAX];
        buf[..len].copy_from_slice(&self.history[next][..len]);
        self.copy_into_cmd(&buf[..len]);
        self.history_cursor = Some(next);
    }

    fn history_next(&mut self) {
        let Some(current) = self.history_cursor else {
            return;
        };
        if current + 1 >= self.history_count {
            let len = self.history_draft_len;
            let mut buf = [0u8; CMD_MAX];
            buf[..len].copy_from_slice(&self.history_draft[..len]);
            self.copy_into_cmd(&buf[..len]);
            self.history_cursor = None;
            return;
        }
        let next = current + 1;
        let len = self.history_lens[next] as usize;
        let mut buf = [0u8; CMD_MAX];
        buf[..len].copy_from_slice(&self.history[next][..len]);
        self.copy_into_cmd(&buf[..len]);
        self.history_cursor = Some(next);
    }

    fn echo_command(&mut self, cmd: &[u8]) {
        let mut line = [0u8; LINE_W];
        let prompt = self.cwd.as_bytes();
        let mut len = 0usize;
        let prompt_len = prompt.len().min(LINE_W.saturating_sub(3));
        line[..prompt_len].copy_from_slice(&prompt[..prompt_len]);
        len += prompt_len;
        line[len] = b'>';
        line[len + 1] = b' ';
        len += 2;
        let copy = cmd.len().min(LINE_W.saturating_sub(len));
        line[len..len + copy].copy_from_slice(&cmd[..copy]);
        self.push_line(&line[..len + copy]);
    }

    fn execute(&mut self) {
        let trimmed = trim_ascii(&self.cmd[..self.cmd_len]);
        let trimmed_len = trimmed.len().min(CMD_MAX);
        let mut owned = [0u8; CMD_MAX];
        owned[..trimmed_len].copy_from_slice(&trimmed[..trimmed_len]);
        let cmd_buf = &owned[..trimmed_len];

        self.echo_command(cmd_buf);
        self.save_history(cmd_buf);
        self.history_cursor = None;

        if cmd_buf.is_empty() {
            self.cmd_len = 0;
            return;
        }

        let (cmd, rest) = split_first_word(cmd_buf);
        match cmd {
            b"help" => self.cmd_help(),
            b"clear" => self.cmd_clear(),
            b"whoami" => self.cmd_whoami(),
            b"uname" => self.cmd_uname(),
            b"time" => self.cmd_time(),
            b"orch" | b"orchestrator" => self.cmd_orchestrator(),
            b"graph" => self.cmd_graph(),
            b"registry" => self.cmd_registry(),
            b"context" | b"ctx" => self.cmd_context(),
            b"focus" => self.cmd_focus(),
            b"version" => self.cmd_version(),
            b"apps" => self.cmd_apps(),
            b"services" => self.cmd_services(),
            b"ps" => self.cmd_ps(),
            b"env" => self.cmd_env(),
            b"pwd" => {
                let cwd = self.cwd;
                self.push_line(cwd.as_bytes());
            }
            b"ls" => self.cmd_ls(rest),
            b"tree" => self.cmd_tree(rest),
            b"cd" => self.cmd_cd(rest),
            b"cat" => self.cmd_cat(rest),
            b"head" => self.cmd_head(rest),
            b"tail" => self.cmd_tail(rest),
            b"wc" => self.cmd_wc(rest),
            b"grep" => self.cmd_grep(rest),
            b"hexdump" => self.cmd_hexdump(rest),
            b"mkdir" => self.cmd_mkdir(rest),
            b"touch" => self.cmd_touch(rest),
            b"rm" => self.cmd_rm(rest),
            b"mv" => self.cmd_mv(rest),
            b"cp" => self.cmd_cp(rest),
            b"echo" => self.cmd_echo(rest),
            b"which" => self.cmd_which(rest),
            b"toolchain" | b"devtools" => self.cmd_toolchain(),
            b"launch" | b"open" => self.cmd_launch(rest),
            b"files" => self.launch_named(b"files"),
            b"editor" => self.launch_named(b"editor"),
            b"code" => self.launch_named(b"editor"),
            b"studio" => self.launch_named(b"editor"),
            b"notes" | b"notepad" => self.launch_named(b"notepad"),
            b"paint" => self.launch_named(b"paint"),
            b"calc" | b"calculator" => self.launch_named(b"calculator"),
            b"settings" => self.launch_named(b"settings"),
            b"launcher" => self.launch_named(b"cube"),
            b"shell" => self.launch_named(b"cube"),
            b"ai" => self.launch_named(b"ai-console"),
            b"copilot" => self.launch_named(b"ai-console"),
            b"ssh" => self.launch_named(b"ssh"),
            b"store" => self.launch_named(b"appstored"),
            _ => {
                self.push_line(b"Unknown command. Try 'help'.");
                self.set_status(b"Unknown command.");
            }
        }

        self.cmd_len = 0;
    }

    fn cmd_help(&mut self) {
        self.push_line(b"Core: help clear whoami uname version time orch graph registry");
        self.push_line(b"Focus: context focus studio copilot shell");
        self.push_line(b"Shell: pwd ls tree cd cat head tail grep wc hexdump echo");
        self.push_line(b"Graph: services ps env apps toolchain which");
        self.push_line(b"Files: mkdir touch rm mv cp");
        self.push_line(b"Apps: launch/open files editor notes paint calc settings ai ssh store");
        self.set_status(b"Shell help loaded.");
    }

    fn cmd_clear(&mut self) {
        self.head = 0;
        self.count = 0;
        self.scroll = 0;
        self.push_line(b"Terminal cleared.");
        self.set_status(b"Output cleared.");
    }

    fn cmd_whoami(&mut self) {
        match runtime::getuid() {
            Some(uid) => {
                let mut buf = [0u8; 32];
                buf[..4].copy_from_slice(b"uid=");
                let len = 4 + write_u32(uid, &mut buf[4..]);
                self.push_line(&buf[..len]);
            }
            None => self.push_line(b"uid unavailable"),
        }
        self.set_status(b"Identity queried.");
    }

    fn cmd_uname(&mut self) {
        self.push_line(b"GraphOS x86_64 ring3");
        self.push_line(b"graph-aware shell + orchestrator time");
        self.set_status(b"Platform identified.");
    }

    fn cmd_time(&mut self) {
        let mut line = [0u8; 48];
        let prefix = b"orchestrator.now=";
        line[..prefix.len()].copy_from_slice(prefix);
        let len = prefix.len() + write_u64(self.now_ms, &mut line[prefix.len()..]);
        line[len] = b'm';
        line[len + 1] = b's';
        self.push_line(&line[..len + 2]);
        self.set_status(b"Orchestrator time reported.");
    }

    fn cmd_orchestrator(&mut self) {
        self.push_line(b"clock source: orchestrator");
        self.push_line(b"desktop tick: frame-tick broadcast");
        self.cmd_time();
        self.set_status(b"Orchestrator status shown.");
    }

    fn cmd_graph(&mut self) {
        self.push_line(b"graph service bindings:");
        let mut i = 0usize;
        while i < CORE_SERVICE_NAMES.len() {
            let name = CORE_SERVICE_NAMES[i];
            let binding = runtime::graph_service_lookup(name);
            let registry = runtime::registry_lookup(name);
            if let Some((stable_id, node_id)) = binding {
                let mut line = [0u8; LINE_W];
                let mut len = 0usize;
                let copy = name.len().min(24);
                line[..copy].copy_from_slice(&name[..copy]);
                len += copy;
                line[len] = b' ';
                line[len + 1] = b'#';
                len += 2;
                len += write_u32(stable_id as u32, &mut line[len..]);
                line[len] = b' ';
                line[len + 1] = b'n';
                line[len + 2] = b'=';
                len += 3;
                len += write_hex_u64(node_id, &mut line[len..]);
                if let Some(info) = registry {
                    line[len] = b' ';
                    line[len + 1] = b'c';
                    line[len + 2] = b'h';
                    line[len + 3] = b'=';
                    len += 4;
                    len += write_u32(info.channel_alias, &mut line[len..]);
                }
                self.push_line(&line[..len]);
            }
            i += 1;
        }
        if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
            let mut line = [0u8; 48];
            let prefix = b"em(1,1) t=";
            line[..prefix.len()].copy_from_slice(prefix);
            let mut len = prefix.len();
            len += write_u32(transitions, &mut line[len..]);
            line[len] = b' ';
            line[len + 1] = b'e';
            line[len + 2] = b'=';
            len += 3;
            len += write_u32(epoch, &mut line[len..]);
            self.push_line(&line[..len]);
        }
        self.set_status(b"Graph bindings listed.");
    }

    fn cmd_registry(&mut self) {
        let generation = runtime::registry_subscribe(0);
        let mut line = [0u8; 48];
        let prefix = b"registry.generation=";
        line[..prefix.len()].copy_from_slice(prefix);
        let len = prefix.len() + write_u64(generation, &mut line[prefix.len()..]);
        self.push_line(&line[..len]);
        self.cmd_services();
        self.set_status(b"Registry state listed.");
    }

    fn cmd_context(&mut self) {
        self.refresh_workspace_context(false);
        self.push_line(b"graph workspace context:");
        let mut line = [0u8; LINE_W];
        let mut len = 0usize;
        line[..6].copy_from_slice(b"scope=");
        len += 6;
        let scope = self.context_scope();
        let copy = scope.len().min(LINE_W.saturating_sub(len));
        line[len..len + copy].copy_from_slice(&scope[..copy]);
        self.push_line(&line[..len + copy]);

        let mut line = [0u8; LINE_W];
        let mut len = 0usize;
        line[..6].copy_from_slice(b"focus=");
        len += 6;
        let focus = self.context_focus();
        let copy = focus.len().min(LINE_W.saturating_sub(len));
        line[len..len + copy].copy_from_slice(&focus[..copy]);
        self.push_line(&line[..len + copy]);

        let mut line = [0u8; LINE_W];
        let mut len = 0usize;
        line[..7].copy_from_slice(b"source=");
        len += 7;
        let source = self.context_source();
        let copy = source.len().min(LINE_W.saturating_sub(len));
        line[len..len + copy].copy_from_slice(&source[..copy]);
        len += copy;
        line[len] = b' ';
        line[len + 1] = b'[';
        len += 2;
        let kind = if self.context_is_dir {
            b"dir".as_slice()
        } else {
            b"file".as_slice()
        };
        line[len..len + kind.len()].copy_from_slice(kind);
        len += kind.len();
        line[len] = b']';
        self.push_line(&line[..len + 1]);
        self.set_status(b"Graph context listed.");
    }

    fn cmd_focus(&mut self) {
        self.refresh_workspace_context(false);
        self.adopt_context_directory();
    }

    fn cmd_version(&mut self) {
        self.push_line(b"GraphOS developer shell");
        self.push_line(b"Desktop path: graph-aware compositor + orchestrator clock");
        self.set_status(b"Version details printed.");
    }

    fn cmd_apps(&mut self) {
        self.push_line(b"Desktop: files editor notepad paint calculator settings terminal");
        self.push_line(b"Developer: ai-console ssh appstored launcher");
        self.push_line(b"Graph: graphd modeld trainerd sysd services");
        self.set_status(b"App catalog listed.");
    }

    fn cmd_services(&mut self) {
        let mut found = false;
        let mut i = 0usize;
        while i < CORE_SERVICE_NAMES.len() {
            let name = CORE_SERVICE_NAMES[i];
            if let Some(info) = runtime::registry_lookup(name) {
                let mut line = [0u8; 64];
                let mut len = 0usize;
                line[..name.len()].copy_from_slice(name);
                len += name.len();
                line[len] = b' ';
                line[len + 1] = b'[';
                len += 2;
                let status: &[u8] = if info.health == 1 {
                    b"ready"
                } else {
                    b"degraded"
                };
                line[len..len + status.len()].copy_from_slice(status);
                len += status.len();
                line[len] = b']';
                len += 1;
                self.push_line(&line[..len]);
                found = true;
            }
            i += 1;
        }
        if !found {
            self.push_line(b"no services registered");
        }
        self.set_status(b"Service state listed.");
    }

    fn cmd_ps(&mut self) {
        self.push_line(b"SERVICE     STATE");
        self.cmd_services();
    }

    fn cmd_env(&mut self) {
        self.push_line(b"SHELL=/bin/graphsh");
        self.push_line(b"GRAPH_ROOT=/graph");
        self.push_line(b"TIME_NODE=orchestrator");
        self.push_line(b"DESKTOP_CLOCK=orchestrator");
        let mut scope = [0u8; MAX_PATH + 12];
        scope[..12].copy_from_slice(b"GRAPH_SCOPE=");
        let scope_copy = self.context_scope().len().min(MAX_PATH);
        scope[12..12 + scope_copy].copy_from_slice(&self.context_scope()[..scope_copy]);
        self.push_line(&scope[..12 + scope_copy]);
        let mut focus = [0u8; MAX_PATH + 12];
        focus[..12].copy_from_slice(b"GRAPH_FOCUS=");
        let focus_copy = self.context_focus().len().min(MAX_PATH);
        focus[12..12 + focus_copy].copy_from_slice(&self.context_focus()[..focus_copy]);
        self.push_line(&focus[..12 + focus_copy]);
        let mut pwd = [0u8; MAX_PATH + 4];
        pwd[..4].copy_from_slice(b"PWD=");
        let copy = self.cwd.len.min(MAX_PATH);
        pwd[4..4 + copy].copy_from_slice(&self.cwd.as_bytes()[..copy]);
        self.push_line(&pwd[..4 + copy]);
        self.set_status(b"Environment printed.");
    }

    fn read_path_into(&self, path: Path, buf: &mut [u8]) -> Option<usize> {
        let fd = runtime::vfs_open(path.as_bytes());
        if fd == u64::MAX {
            return None;
        }
        let bytes = runtime::vfs_read(fd, buf) as usize;
        runtime::vfs_close(fd);
        Some(bytes)
    }

    fn cmd_ls(&mut self, rest: &[u8]) {
        let path = if rest.is_empty() {
            self.cwd
        } else {
            self.resolve_path(rest)
        };
        let fd = runtime::vfs_open(path.as_bytes());
        if fd == u64::MAX {
            self.push_line(b"ls: path unavailable");
            self.set_status(b"ls failed.");
            return;
        }
        let mut buf = [0u8; 1536];
        let bytes = runtime::vfs_read(fd, &mut buf) as usize;
        runtime::vfs_close(fd);
        if bytes == 0 {
            self.push_line(b"(empty)");
            self.set_status(b"Directory listed.");
            return;
        }
        let mut cursor = 0usize;
        let mut shown = 0usize;
        while cursor < bytes && shown < 40 {
            let start = cursor;
            while cursor < bytes && buf[cursor] != b'\n' && buf[cursor] != 0 {
                cursor += 1;
            }
            let raw = &buf[start..cursor];
            if !raw.is_empty() {
                let is_dir = raw[0] == b'd';
                let name = if is_dir && raw.len() > 1 {
                    &raw[1..]
                } else {
                    raw
                };
                let mut line = [0u8; LINE_W];
                line[0] = if is_dir { b'd' } else { b'f' };
                line[1] = b' ';
                let copy = name.len().min(LINE_W - 2);
                line[2..2 + copy].copy_from_slice(&name[..copy]);
                self.push_line(&line[..2 + copy]);
                shown += 1;
            }
            cursor += 1;
        }
        if shown == 40 && cursor < bytes {
            self.push_line(b"... output truncated ...");
        }
        self.set_status(b"Directory listed.");
    }

    fn cmd_tree(&mut self, rest: &[u8]) {
        let path = if rest.is_empty() {
            self.cwd
        } else {
            self.resolve_path(rest)
        };
        self.push_line(path.as_bytes());
        let mut buf = [0u8; 1536];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"tree: path unavailable");
            self.set_status(b"Tree failed.");
            return;
        };
        if bytes == 0 {
            self.push_line(b"`-- (empty)");
            self.set_status(b"Tree listed.");
            return;
        }
        let mut cursor = 0usize;
        let mut shown = 0usize;
        while cursor < bytes && shown < 32 {
            let start = cursor;
            while cursor < bytes && buf[cursor] != b'\n' && buf[cursor] != 0 {
                cursor += 1;
            }
            let raw = &buf[start..cursor];
            if !raw.is_empty() {
                let is_dir = raw[0] == b'd';
                let name = if is_dir && raw.len() > 1 {
                    &raw[1..]
                } else {
                    raw
                };
                let mut line = [0u8; LINE_W];
                line[..4].copy_from_slice(b"|-- ");
                let copy = name.len().min(LINE_W - 6);
                line[4..4 + copy].copy_from_slice(&name[..copy]);
                if is_dir && 4 + copy + 1 < LINE_W {
                    line[4 + copy] = b'/';
                    self.push_line(&line[..5 + copy]);
                } else {
                    self.push_line(&line[..4 + copy]);
                }
                shown += 1;
            }
            cursor += 1;
        }
        if shown == 32 && cursor < bytes {
            self.push_line(b"`-- ...");
        }
        self.set_status(b"Tree listed.");
    }

    fn cmd_cd(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.cwd = Path::from_bytes(self.context_scope());
            let mut cwd = [0u8; MAX_PATH];
            let cwd_bytes = self.cwd.as_bytes();
            let cwd_len = cwd_bytes.len().min(cwd.len());
            cwd[..cwd_len].copy_from_slice(&cwd_bytes[..cwd_len]);
            self.push_line(&cwd[..cwd_len]);
            self.sync_workspace_context();
            self.set_status(b"Moved to graph scope.");
            return;
        }
        let path = self.resolve_path(rest);
        let fd = runtime::vfs_open(path.as_bytes());
        if fd == u64::MAX {
            self.push_line(b"cd: path unavailable");
            self.set_status(b"cd failed.");
            return;
        }
        let mut buf = [0u8; 256];
        let bytes = runtime::vfs_read(fd, &mut buf) as usize;
        runtime::vfs_close(fd);
        if bytes > 0 && !looks_like_dir_listing(&buf[..bytes]) {
            self.push_line(b"cd: not a directory");
            self.set_status(b"cd failed.");
            return;
        }
        self.cwd = path;
        let cwd = self.cwd;
        self.push_line(cwd.as_bytes());
        self.sync_workspace_context();
        self.set_status(b"Directory changed.");
    }

    fn cmd_cat(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"cat: missing path");
            self.set_status(b"cat needs a file.");
            return;
        }
        let path = self.resolve_path(rest);
        let mut buf = [0u8; 512];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"cat: path unavailable");
            self.set_status(b"cat failed.");
            return;
        };
        if bytes == 0 {
            self.push_line(b"(empty)");
            self.set_status(b"File displayed.");
            return;
        }
        if preview_is_text(&buf[..bytes]) {
            let mut start = 0usize;
            let mut lines = 0usize;
            while start <= bytes && lines < 16 {
                let mut end = start;
                while end < bytes && buf[end] != b'\n' {
                    end += 1;
                }
                self.push_line(&buf[start..end]);
                lines += 1;
                if end >= bytes {
                    break;
                }
                start = end + 1;
            }
        } else {
            let mut offset = 0usize;
            while offset < bytes {
                let take = (bytes - offset).min(8);
                let mut line = [b' '; 40];
                let mut len = write_hex_u16(offset as u16, &mut line);
                line[len] = b':';
                line[len + 1] = b' ';
                len += 2;
                let mut i = 0usize;
                while i < take {
                    let byte = buf[offset + i];
                    line[len] = nybble(byte >> 4);
                    line[len + 1] = nybble(byte);
                    line[len + 2] = b' ';
                    len += 3;
                    i += 1;
                }
                self.push_line(&line[..len]);
                offset += take;
            }
        }
        self.set_status(b"File displayed.");
    }

    fn cmd_head(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"head: missing path");
            self.set_status(b"head needs a file.");
            return;
        }
        let path = self.resolve_path(rest);
        let mut buf = [0u8; 1024];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"head: path unavailable");
            self.set_status(b"head failed.");
            return;
        };
        if !preview_is_text(&buf[..bytes]) {
            self.push_line(b"head: text files only");
            self.set_status(b"head failed.");
            return;
        }
        let mut start = 0usize;
        let mut lines = 0usize;
        while start <= bytes && lines < 10 {
            let mut end = start;
            while end < bytes && buf[end] != b'\n' {
                end += 1;
            }
            self.push_line(&buf[start..end]);
            lines += 1;
            if end >= bytes {
                break;
            }
            start = end + 1;
        }
        self.set_status(b"Head printed.");
    }

    fn cmd_tail(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"tail: missing path");
            self.set_status(b"tail needs a file.");
            return;
        }
        let path = self.resolve_path(rest);
        let mut buf = [0u8; 1536];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"tail: path unavailable");
            self.set_status(b"tail failed.");
            return;
        };
        if !preview_is_text(&buf[..bytes]) {
            self.push_line(b"tail: text files only");
            self.set_status(b"tail failed.");
            return;
        }
        let mut total_lines = 0usize;
        let mut i = 0usize;
        while i < bytes {
            if buf[i] == b'\n' {
                total_lines += 1;
            }
            i += 1;
        }
        let target = total_lines.saturating_sub(9);
        let mut line = 0usize;
        let mut start = 0usize;
        while start <= bytes {
            let mut end = start;
            while end < bytes && buf[end] != b'\n' {
                end += 1;
            }
            if line >= target {
                self.push_line(&buf[start..end]);
            }
            if end >= bytes {
                break;
            }
            start = end + 1;
            line += 1;
        }
        self.set_status(b"Tail printed.");
    }

    fn cmd_wc(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"wc: missing path");
            self.set_status(b"wc needs a file.");
            return;
        }
        let path = self.resolve_path(rest);
        let mut buf = [0u8; 2048];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"wc: path unavailable");
            self.set_status(b"wc failed.");
            return;
        };
        let mut lines = 0u32;
        let mut words = 0u32;
        let mut in_word = false;
        let mut i = 0usize;
        while i < bytes {
            let b = buf[i];
            if b == b'\n' {
                lines += 1;
            }
            let is_space = matches!(b, b' ' | b'\n' | b'\r' | b'\t');
            if !is_space && !in_word {
                words += 1;
                in_word = true;
            } else if is_space {
                in_word = false;
            }
            i += 1;
        }
        let mut line = [0u8; 48];
        let mut len = write_u32(lines, &mut line);
        line[len] = b' ';
        len += 1;
        len += write_u32(words, &mut line[len..]);
        line[len] = b' ';
        len += 1;
        len += write_u32(bytes as u32, &mut line[len..]);
        self.push_line(&line[..len]);
        self.set_status(b"Counts printed.");
    }

    fn cmd_grep(&mut self, rest: &[u8]) {
        let (needle, path_raw) = split_two_words(rest);
        if needle.is_empty() || path_raw.is_empty() {
            self.push_line(b"grep: need text and file");
            self.set_status(b"grep needs a needle and file.");
            return;
        }
        let path = self.resolve_path(path_raw);
        let mut buf = [0u8; 1536];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"grep: path unavailable");
            self.set_status(b"grep failed.");
            return;
        };
        if !preview_is_text(&buf[..bytes]) {
            self.push_line(b"grep: text files only");
            self.set_status(b"grep failed.");
            return;
        }
        let mut start = 0usize;
        let mut found = false;
        while start <= bytes {
            let mut end = start;
            while end < bytes && buf[end] != b'\n' {
                end += 1;
            }
            let line = &buf[start..end];
            if contains_subslice(line, needle) {
                self.push_line(line);
                found = true;
            }
            if end >= bytes {
                break;
            }
            start = end + 1;
        }
        if !found {
            self.push_line(b"(no matches)");
        }
        self.set_status(b"Grep completed.");
    }

    fn cmd_hexdump(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"hexdump: missing path");
            self.set_status(b"hexdump needs a file.");
            return;
        }
        let path = self.resolve_path(rest);
        let mut buf = [0u8; 256];
        let Some(bytes) = self.read_path_into(path, &mut buf) else {
            self.push_line(b"hexdump: path unavailable");
            self.set_status(b"hexdump failed.");
            return;
        };
        let mut offset = 0usize;
        while offset < bytes {
            let take = (bytes - offset).min(8);
            let mut line = [b' '; 40];
            let mut len = write_hex_u16(offset as u16, &mut line);
            line[len] = b':';
            line[len + 1] = b' ';
            len += 2;
            let mut i = 0usize;
            while i < take {
                let byte = buf[offset + i];
                line[len] = nybble(byte >> 4);
                line[len + 1] = nybble(byte);
                line[len + 2] = b' ';
                len += 3;
                i += 1;
            }
            self.push_line(&line[..len]);
            offset += take;
        }
        self.set_status(b"Hexdump printed.");
    }

    fn cmd_mkdir(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"mkdir: missing path");
            self.set_status(b"mkdir needs a directory name.");
            return;
        }
        let path = self.resolve_path(rest);
        if runtime::vfs_mkdir(path.as_bytes()) {
            self.push_line(b"directory created");
            self.set_status(b"Directory created.");
        } else {
            self.push_line(b"mkdir failed");
            self.set_status(b"Directory create failed.");
        }
    }

    fn cmd_touch(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"touch: missing path");
            self.set_status(b"touch needs a file name.");
            return;
        }
        let path = self.resolve_path(rest);
        let fd = runtime::vfs_create(path.as_bytes());
        if fd == u64::MAX {
            self.push_line(b"touch failed");
            self.set_status(b"File create failed.");
            return;
        }
        runtime::vfs_close(fd);
        self.push_line(b"file created");
        self.set_status(b"File created.");
    }

    fn cmd_rm(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"rm: missing path");
            self.set_status(b"rm needs a path.");
            return;
        }
        let path = self.resolve_path(rest);
        if runtime::vfs_unlink(path.as_bytes()) {
            self.push_line(b"path removed");
            self.set_status(b"Path removed.");
        } else {
            self.push_line(b"rm failed");
            self.set_status(b"Path remove failed.");
        }
    }

    fn cmd_mv(&mut self, rest: &[u8]) {
        let (src, dst) = split_two_words(rest);
        if src.is_empty() || dst.is_empty() {
            self.push_line(b"mv: need src and dst");
            self.set_status(b"mv needs two paths.");
            return;
        }
        let src_path = self.resolve_path(src);
        let dst_path = self.resolve_path(dst);
        if runtime::vfs_rename(src_path.as_bytes(), dst_path.as_bytes()) {
            self.push_line(b"path renamed");
            self.set_status(b"Rename complete.");
        } else {
            self.push_line(b"mv failed");
            self.set_status(b"Rename failed.");
        }
    }

    fn cmd_cp(&mut self, rest: &[u8]) {
        let (src, dst) = split_two_words(rest);
        if src.is_empty() || dst.is_empty() {
            self.push_line(b"cp: need src and dst");
            self.set_status(b"cp needs two paths.");
            return;
        }
        let src_path = self.resolve_path(src);
        let dst_path = self.resolve_path(dst);
        let src_fd = runtime::vfs_open(src_path.as_bytes());
        if src_fd == u64::MAX {
            self.push_line(b"cp: source unavailable");
            self.set_status(b"Copy failed.");
            return;
        }
        let dst_fd = runtime::vfs_create(dst_path.as_bytes());
        if dst_fd == u64::MAX {
            runtime::vfs_close(src_fd);
            self.push_line(b"cp: destination unavailable");
            self.set_status(b"Copy failed.");
            return;
        }
        let mut buf = [0u8; 512];
        loop {
            let read = runtime::vfs_read(src_fd, &mut buf) as usize;
            if read == 0 {
                break;
            }
            let wrote = runtime::vfs_write(dst_fd, &buf[..read]) as usize;
            if wrote != read {
                self.push_line(b"cp: write interrupted");
                self.set_status(b"Copy failed.");
                runtime::vfs_close(src_fd);
                runtime::vfs_close(dst_fd);
                return;
            }
            if read < buf.len() {
                break;
            }
        }
        runtime::vfs_close(src_fd);
        runtime::vfs_close(dst_fd);
        self.push_line(b"file copied");
        self.set_status(b"Copy complete.");
    }

    fn cmd_echo(&mut self, rest: &[u8]) {
        self.push_line(rest);
        self.set_status(b"Echo printed.");
    }

    fn cmd_which(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"which: missing command");
            self.set_status(b"which needs a command.");
            return;
        }
        if is_builtin(rest) {
            let mut line = [0u8; 40];
            line[..8].copy_from_slice(b"builtin ");
            let copy = rest.len().min(32);
            line[8..8 + copy].copy_from_slice(&rest[..copy]);
            self.push_line(&line[..8 + copy]);
            self.set_status(b"Builtin command found.");
            return;
        }
        if is_app(rest) {
            let mut line = [0u8; 40];
            line[..4].copy_from_slice(b"app ");
            let copy = rest.len().min(36);
            line[4..4 + copy].copy_from_slice(&rest[..copy]);
            self.push_line(&line[..4 + copy]);
            self.set_status(b"App command found.");
        } else {
            self.push_line(b"not found");
            self.set_status(b"Command not found.");
        }
    }

    fn cmd_toolchain(&mut self) {
        self.push_line(b"Graph: time orch graph registry services ps env");
        self.push_line(b"Focus: context focus studio copilot shell");
        self.push_line(b"Shell: pwd ls tree cd cat head tail grep wc hexdump");
        self.push_line(b"Files: mkdir touch rm mv cp echo which clear");
        self.push_line(b"Developer apps: editor notepad files ai-console ssh");
        self.push_line(b"Desktop apps: paint calculator settings terminal appstored");
        self.set_status(b"Toolchain listed.");
    }

    fn cmd_launch(&mut self, rest: &[u8]) {
        if rest.is_empty() {
            self.push_line(b"launch: missing app");
            self.set_status(b"Launch needs an app name.");
            return;
        }
        self.launch_named(rest);
    }

    fn launch_named(&mut self, app: &[u8]) {
        self.sync_workspace_context();
        if runtime::spawn_named_checked(app) {
            self.push_line(b"launch dispatched");
            self.set_status(b"Application launched.");
        } else {
            self.push_line(b"launch failed");
            self.set_status(b"Application launch failed.");
        }
    }

    fn resolve_path(&self, raw: &[u8]) -> Path {
        let raw = trim_ascii(raw);
        let mut path = if raw.first() == Some(&b'/') {
            Path::root()
        } else {
            self.cwd
        };
        let mut start = 0usize;
        while start < raw.len() {
            while start < raw.len() && raw[start] == b'/' {
                start += 1;
            }
            if start >= raw.len() {
                break;
            }
            let mut end = start;
            while end < raw.len() && raw[end] != b'/' {
                end += 1;
            }
            let part = &raw[start..end];
            if part == b"." {
                // stay
            } else if part == b".." {
                path = path.parent();
            } else {
                path = path.join(part);
            }
            start = end + 1;
        }
        path
    }

    fn run_quick_action(&mut self, index: usize) {
        match index {
            0 => self.launch_named(b"files"),
            1 => self.launch_named(b"editor"),
            2 => self.launch_named(b"notepad"),
            3 => self.launch_named(b"paint"),
            4 => self.launch_named(b"calculator"),
            5 => self.launch_named(b"settings"),
            6 => self.launch_named(b"ai-console"),
            7 => self.launch_named(b"ssh"),
            _ => {}
        }
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        let dirty = true;
        if left_down && !left_prev {
            let mut idx = 0usize;
            while idx < ACTION_COUNT {
                if contains(action_rect(idx), x, y) {
                    self.run_quick_action(idx);
                    break;
                }
                idx += 1;
            }
        }
        self.prev_buttons = buttons;
        dirty
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < bytes.len() && bytes[start] == b' ' {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && bytes[end - 1] == b' ' {
        end -= 1;
    }
    &bytes[start..end]
}

fn split_first_word(bytes: &[u8]) -> (&[u8], &[u8]) {
    let bytes = trim_ascii(bytes);
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx] != b' ' {
        idx += 1;
    }
    if idx >= bytes.len() {
        return (bytes, b"");
    }
    (&bytes[..idx], trim_ascii(&bytes[idx + 1..]))
}

fn split_two_words(bytes: &[u8]) -> (&[u8], &[u8]) {
    let (first, rest) = split_first_word(bytes);
    let (second, _) = split_first_word(rest);
    (first, second)
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let mut start = 0usize;
    while start + needle.len() <= haystack.len() {
        if &haystack[start..start + needle.len()] == needle {
            return true;
        }
        start += 1;
    }
    false
}

fn preview_is_text(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(256)
        .all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..=0x7E).contains(&b))
}

fn looks_like_dir_listing(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let mut cursor = 0usize;
    let mut lines = 0usize;
    while cursor < bytes.len() && lines < 8 {
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'\n' && bytes[cursor] != 0 {
            cursor += 1;
        }
        let raw = &bytes[start..cursor];
        if raw.is_empty() {
            break;
        }
        let name = if raw[0] == b'd' && raw.len() > 1 {
            &raw[1..]
        } else {
            raw
        };
        if name.is_empty()
            || name.len() > 48
            || name
                .iter()
                .any(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/')))
        {
            return false;
        }
        cursor += 1;
        lines += 1;
    }
    true
}

fn replace_bytes(dst: &mut [u8], len: &mut usize, src: &[u8]) -> bool {
    let next = src.len().min(dst.len());
    if *len == next && &dst[..next] == &src[..next] {
        return false;
    }
    dst[..next].copy_from_slice(&src[..next]);
    *len = next;
    true
}

fn contains(rect: Rect, x: i16, y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    x >= rect.x && y >= rect.y && x < rect.x + rect.w as i32 && y < rect.y + rect.h as i32
}

fn action_rect(index: usize) -> Rect {
    let panel_x = (WIN_W - SIDEPANEL_W + 12) as i32;
    let panel_y = (HEADER_H + METRICS_H + 28) as i32;
    Rect::new(panel_x, panel_y + index as i32 * 38, SIDEPANEL_W - 24, 28)
}

fn is_builtin(name: &[u8]) -> bool {
    matches!(
        name,
        b"help"
            | b"clear"
            | b"whoami"
            | b"uname"
            | b"time"
            | b"orch"
            | b"orchestrator"
            | b"graph"
            | b"registry"
            | b"context"
            | b"ctx"
            | b"focus"
            | b"version"
            | b"services"
            | b"ps"
            | b"env"
            | b"apps"
            | b"toolchain"
            | b"devtools"
            | b"pwd"
            | b"ls"
            | b"tree"
            | b"cd"
            | b"cat"
            | b"head"
            | b"tail"
            | b"wc"
            | b"grep"
            | b"hexdump"
            | b"mkdir"
            | b"touch"
            | b"rm"
            | b"mv"
            | b"cp"
            | b"echo"
            | b"which"
            | b"launch"
            | b"open"
            | b"files"
            | b"editor"
            | b"code"
            | b"studio"
            | b"notes"
            | b"notepad"
            | b"paint"
            | b"calc"
            | b"calculator"
            | b"settings"
            | b"launcher"
            | b"shell"
            | b"ai"
            | b"copilot"
            | b"ssh"
            | b"store"
    )
}

fn is_app(name: &[u8]) -> bool {
    matches!(
        name,
        b"files"
            | b"editor"
            | b"studio"
            | b"notepad"
            | b"paint"
            | b"calculator"
            | b"settings"
            | b"launcher"
            | b"shell"
            | b"terminal"
            | b"ai-console"
            | b"copilot"
            | b"ssh"
            | b"appstored"
    )
}

fn build_prompt(cwd: &[u8], out: &mut [u8; MAX_PATH + 4]) -> usize {
    let copy = cwd.len().min(MAX_PATH);
    out[..copy].copy_from_slice(&cwd[..copy]);
    out[copy] = b'>';
    out[copy + 1] = b' ';
    copy + 2
}

fn draw(win: &mut Window, term: &Terminal) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    let root = Rect::new(0, 0, WIN_W, WIN_H);
    draw_window_frame(&mut canvas, root, b"GraphOS Terminal", THEME);

    let metrics_y = HEADER_H as i32;
    let card_w = (WIN_W - 24) / 4;
    draw_stat_card(
        &mut canvas,
        Rect::new(8, metrics_y + 8, card_w - 6, 38),
        b"Mode",
        b"Graph-first shell",
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(14 + card_w as i32, metrics_y + 8, card_w - 6, 38),
        b"Scope",
        term.context_scope(),
        palette.success,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(20 + card_w as i32 * 2, metrics_y + 8, card_w - 6, 38),
        b"Clock",
        tick_label(term.now_ms),
        palette.warning,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(26 + card_w as i32 * 3, metrics_y + 8, card_w - 6, 38),
        b"Source",
        term.context_source(),
        palette.text_muted,
        THEME,
    );

    let body_y = (HEADER_H + METRICS_H) as i32;
    let output_panel = Rect::new(
        0,
        body_y,
        WIN_W - SIDEPANEL_W,
        WIN_H - HEADER_H - METRICS_H - COMMAND_H,
    );
    let side_panel = Rect::new(output_panel.w as i32, body_y, SIDEPANEL_W, output_panel.h);

    let output_rect = draw_panel(&mut canvas, output_panel, b"Console", THEME);
    let side_rect = draw_panel(&mut canvas, side_panel, b"Tools", THEME);

    let mut rel = 0usize;
    while rel < LINES_VISIBLE {
        let abs = term.scroll + rel;
        if abs >= term.count {
            break;
        }
        canvas.draw_text(
            output_rect.x,
            output_rect.y + rel as i32 * LINE_H as i32,
            term.line(abs),
            palette.text,
            output_rect.w.saturating_sub(16),
        );
        rel += 1;
    }

    draw_scroll_track(
        &mut canvas,
        Rect::new(
            output_rect.x + output_rect.w as i32 - 8,
            output_rect.y,
            8,
            output_rect.h,
        ),
        (term.count as u32).saturating_mul(LINE_H),
        output_rect.h,
        (term.scroll as u32).saturating_mul(LINE_H),
        THEME,
    );

    let mut i = 0usize;
    while i < ACTION_COUNT {
        let hovered = contains(action_rect(i), term.pointer_x, term.pointer_y);
        draw_button(
            &mut canvas,
            action_rect(i),
            ACTION_LABELS[i],
            if i == 4 || i == 5 {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            false,
            hovered,
            false,
            THEME,
        );
        i += 1;
    }

    draw_stat_card(
        &mut canvas,
        Rect::new(side_rect.x, side_rect.y + 248, side_rect.w, 40),
        b"Graph Focus",
        term.context_focus(),
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(side_rect.x, side_rect.y + 294, side_rect.w, 40),
        b"History",
        history_count_label(term),
        palette.success,
        THEME,
    );
    canvas.draw_text(
        side_rect.x,
        side_rect.y + 346,
        b"Commands",
        palette.text_muted,
        side_rect.w,
    );
    canvas.draw_text(
        side_rect.x,
        side_rect.y + 364,
        b"context focus studio copilot",
        palette.text,
        side_rect.w,
    );
    canvas.draw_text(
        side_rect.x,
        side_rect.y + 380,
        b"time orch graph registry",
        palette.text,
        side_rect.w,
    );
    canvas.draw_text(
        side_rect.x,
        side_rect.y + 396,
        b"pwd ls tree cd cat grep wc",
        palette.text,
        side_rect.w,
    );
    canvas.draw_text(
        side_rect.x,
        side_rect.y + 424,
        b"Recent",
        palette.text_muted,
        side_rect.w,
    );

    let start = term.history_count.saturating_sub(4);
    let mut row = 0usize;
    let mut idx = start;
    while idx < term.history_count {
        let len = term.history_lens[idx] as usize;
        canvas.draw_text(
            side_rect.x,
            side_rect.y + 442 + row as i32 * 14,
            &term.history[idx][..len],
            palette.text,
            side_rect.w,
        );
        idx += 1;
        row += 1;
    }

    canvas.draw_text(
        side_rect.x,
        side_rect.y + side_rect.h as i32 - 14,
        &term.status[..term.status_len],
        palette.text_muted,
        side_rect.w,
    );

    let cursor_on = ((term.now_ms / 400) % 2) == 0;
    let mut prompt = [0u8; MAX_PATH + 4];
    let prompt_len = build_prompt(term.cwd.as_bytes(), &mut prompt);
    draw_command_bar(
        &mut canvas,
        Rect::new(0, (WIN_H - COMMAND_H) as i32, WIN_W, COMMAND_H),
        &prompt[..prompt_len],
        &term.cmd[..term.cmd_len],
        cursor_on,
        THEME,
    );

    win.present();
}

fn tick_label(now_ms: u64) -> &'static [u8] {
    match now_ms / 1000 {
        0 => b"boot",
        1 => b"1s",
        2 => b"2s",
        3 => b"3s",
        4 => b"4s",
        5..=9 => b"5s+",
        10..=59 => b"10s+",
        _ => b"live",
    }
}

fn history_count_label(term: &Terminal) -> &[u8] {
    match term.history_count {
        0 => b"Empty",
        1..=9 => {
            const LABELS: [&[u8]; 10] = [
                b"Empty",
                b"1 recall",
                b"2 recall",
                b"3 recall",
                b"4 recall",
                b"5 recall",
                b"6 recall",
                b"7 recall",
                b"8 recall",
                b"9 recall",
            ];
            LABELS[term.history_count]
        }
        _ => b"10+ recall",
    }
}

fn write_u32(mut value: u32, out: &mut [u8]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut len = 0usize;
    while value > 0 {
        tmp[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
    }
    let mut i = 0usize;
    while i < len {
        out[i] = tmp[len - 1 - i];
        i += 1;
    }
    len
}

fn write_u64(mut value: u64, out: &mut [u8]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        tmp[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
    }
    let mut i = 0usize;
    while i < len {
        out[i] = tmp[len - 1 - i];
        i += 1;
    }
    len
}

fn nybble(value: u8) -> u8 {
    match value & 0x0F {
        0..=9 => b'0' + (value & 0x0F),
        v => b'A' + (v - 10),
    }
}

fn write_hex_u16(value: u16, out: &mut [u8]) -> usize {
    let bytes = value.to_be_bytes();
    out[0] = nybble(bytes[0] >> 4);
    out[1] = nybble(bytes[0]);
    out[2] = nybble(bytes[1] >> 4);
    out[3] = nybble(bytes[1]);
    4
}

fn write_hex_u64(value: u64, out: &mut [u8]) -> usize {
    let bytes = value.to_be_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        out[i * 2] = nybble(bytes[i] >> 4);
        out[i * 2 + 1] = nybble(bytes[i]);
        i += 1;
    }
    16
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[terminal] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut term = Terminal::new();
    draw(&mut win, &term);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::PointerMove { x, y, buttons } => {
                if term.handle_pointer(x, y, buttons) {
                    draw(&mut win, &term);
                }
            }
            Event::FrameTick { now_ms } => {
                let old_phase = ((term.now_ms / 400) % 2) == 0;
                let new_phase = ((now_ms / 400) % 2) == 0;
                let refresh_due = term.last_context_refresh_ms == 0
                    || now_ms.saturating_sub(term.last_context_refresh_ms) >= 1000;
                term.now_ms = now_ms;
                let context_changed = if refresh_due {
                    term.last_context_refresh_ms = now_ms;
                    term.refresh_workspace_context(false)
                } else {
                    false
                };
                if old_phase != new_phase || context_changed {
                    draw(&mut win, &term);
                }
            }
            Event::Key {
                pressed: true,
                ascii,
                hid_usage,
            } => {
                let dirty = match ascii {
                    0x08 => {
                        if term.cmd_len > 0 {
                            term.cmd_len -= 1;
                            true
                        } else {
                            false
                        }
                    }
                    0x0D | 0x0A => {
                        term.execute();
                        true
                    }
                    0x20..=0x7E if term.cmd_len < CMD_MAX => {
                        term.cmd[term.cmd_len] = ascii;
                        term.cmd_len += 1;
                        term.history_cursor = None;
                        true
                    }
                    0x1B => runtime::exit(0),
                    _ => match hid_usage {
                        0x52 => {
                            term.history_prev();
                            true
                        }
                        0x51 => {
                            term.history_next();
                            true
                        }
                        _ => false,
                    },
                };
                if dirty {
                    draw(&mut win, &term);
                }
            }
            Event::None => runtime::yield_now(),
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[terminal] panic\n");
    runtime::exit(255)
}
