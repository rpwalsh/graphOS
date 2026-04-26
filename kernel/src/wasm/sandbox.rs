// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! WASM MVP opcode interpreter + per-module capability enforcement.
//!
//! Implements the WebAssembly MVP instruction set inside the kernel for
//! sandboxed `.gapp` execution.  Each module gets an isolated linear memory
//! (up to 4 MiB by default) and a capability bitmask that restricts which
//! WASI imports it may invoke.

// ── Capability flags ──────────────────────────────────────────────────────────

/// WASI capability flags (bitmask stored in `WasmModule.capabilities`).
pub const CAP_STDOUT: u64 = 1 << 0;
pub const CAP_STDIN: u64 = 1 << 1;
pub const CAP_FS_READ: u64 = 1 << 2;
pub const CAP_FS_WRITE: u64 = 1 << 3;
pub const CAP_NET: u64 = 1 << 4;
pub const CAP_SYSCALL: u64 = 1 << 5;

// ── Value types ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(u32), // bit-pattern
    F64(u64), // bit-pattern
}

impl Val {
    fn as_i32(self) -> i32 {
        match self {
            Val::I32(v) => v,
            _ => 0,
        }
    }
    fn as_i64(self) -> i64 {
        match self {
            Val::I64(v) => v,
            _ => 0,
        }
    }
}

// ── Interpreter stack ─────────────────────────────────────────────────────────

const MAX_STACK: usize = 512;
const MAX_LOCALS: usize = 64;
const MAX_LABEL_STACK: usize = 64;

struct InterpState {
    stack: [Val; MAX_STACK],
    sp: usize,
    locals: [Val; MAX_LOCALS],
    // Label stack for structured control flow (block/loop/if nesting depth).
    label_pc: [usize; MAX_LABEL_STACK],
    label_sp: usize,
}

impl InterpState {
    const fn new() -> Self {
        Self {
            stack: [Val::I32(0); MAX_STACK],
            sp: 0,
            locals: [Val::I32(0); MAX_LOCALS],
            label_pc: [0; MAX_LABEL_STACK],
            label_sp: 0,
        }
    }
    fn push(&mut self, v: Val) -> bool {
        if self.sp >= MAX_STACK {
            return false;
        }
        self.stack[self.sp] = v;
        self.sp += 1;
        true
    }
    fn pop(&mut self) -> Option<Val> {
        if self.sp == 0 {
            return None;
        }
        self.sp -= 1;
        Some(self.stack[self.sp])
    }
    fn top(&self) -> Option<Val> {
        if self.sp == 0 {
            return None;
        }
        Some(self.stack[self.sp - 1])
    }
}

// ── Opcode constants (Wasm MVP) ───────────────────────────────────────────────

const OP_UNREACHABLE: u8 = 0x00;
const OP_NOP: u8 = 0x01;
const OP_BLOCK: u8 = 0x02;
const OP_LOOP: u8 = 0x03;
const OP_IF: u8 = 0x04;
const OP_ELSE: u8 = 0x05;
const OP_END: u8 = 0x0B;
const OP_BR: u8 = 0x0C;
const OP_BR_IF: u8 = 0x0D;
const OP_RETURN: u8 = 0x0F;
const OP_CALL: u8 = 0x10;
const OP_DROP: u8 = 0x1A;
const OP_SELECT: u8 = 0x1B;
const OP_LOCAL_GET: u8 = 0x20;
const OP_LOCAL_SET: u8 = 0x21;
const OP_LOCAL_TEE: u8 = 0x22;
const OP_I32_LOAD: u8 = 0x28;
const OP_I32_STORE: u8 = 0x36;
const OP_I32_CONST: u8 = 0x41;
const OP_I64_CONST: u8 = 0x42;
const OP_I32_EQZ: u8 = 0x45;
const OP_I32_EQ: u8 = 0x46;
const OP_I32_NE: u8 = 0x47;
const OP_I32_LT_S: u8 = 0x48;
const OP_I32_LT_U: u8 = 0x49;
const OP_I32_GT_S: u8 = 0x4A;
const OP_I32_GT_U: u8 = 0x4B;
const OP_I32_LE_S: u8 = 0x4C;
const OP_I32_LE_U: u8 = 0x4D;
const OP_I32_GE_S: u8 = 0x4E;
const OP_I32_GE_U: u8 = 0x4F;
const OP_I32_CLZ: u8 = 0x67;
const OP_I32_CTZ: u8 = 0x68;
const OP_I32_POPCNT: u8 = 0x69;
const OP_I32_ADD: u8 = 0x6A;
const OP_I32_SUB: u8 = 0x6B;
const OP_I32_MUL: u8 = 0x6C;
const OP_I32_DIV_S: u8 = 0x6D;
const OP_I32_DIV_U: u8 = 0x6E;
const OP_I32_REM_S: u8 = 0x6F;
const OP_I32_REM_U: u8 = 0x70;
const OP_I32_AND: u8 = 0x71;
const OP_I32_OR: u8 = 0x72;
const OP_I32_XOR: u8 = 0x73;
const OP_I32_SHL: u8 = 0x74;
const OP_I32_SHR_S: u8 = 0x75;
const OP_I32_SHR_U: u8 = 0x76;
const OP_I32_ROTL: u8 = 0x77;
const OP_I32_ROTR: u8 = 0x78;

/// Trap reason for a WASM execution failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    Unreachable,
    StackOverflow,
    StackUnderflow,
    MemoryOob,
    DivisionByZero,
    CapabilityDenied,
    UnsupportedOpcode,
    NotImplemented,
}

/// Execute a slice of WASM bytecode with the given capabilities and linear memory.
/// Returns `Ok(())` on clean return/end, `Err(Trap)` on any fault.
///
/// This is a simplified single-function interpreter (no module-level call graph).
/// The full module runner in `mod.rs` dispatches to this per code-body.
pub fn execute(code: &[u8], memory: &mut [u8], caps: u64, locals: &mut [Val]) -> Result<(), Trap> {
    let mut s = InterpState::new();
    // Copy provided locals.
    let local_count = locals.len().min(MAX_LOCALS);
    s.locals[..local_count].copy_from_slice(&locals[..local_count]);

    let mut pc: usize = 0;

    while pc < code.len() {
        let op = code[pc];
        pc += 1;

        match op {
            OP_UNREACHABLE => return Err(Trap::Unreachable),
            OP_NOP => {}

            OP_BLOCK | OP_LOOP => {
                pc += 1; // skip blocktype byte
                if s.label_sp >= MAX_LABEL_STACK {
                    return Err(Trap::StackOverflow);
                }
                s.label_pc[s.label_sp] = pc;
                s.label_sp += 1;
            }

            OP_END => {
                if s.label_sp > 0 {
                    s.label_sp -= 1;
                }
                // Function-level end terminates execution.
                if s.label_sp == 0 {
                    return Ok(());
                }
            }

            OP_IF => {
                pc += 1; // skip blocktype
                let cond = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                if s.label_sp >= MAX_LABEL_STACK {
                    return Err(Trap::StackOverflow);
                }
                s.label_pc[s.label_sp] = pc;
                s.label_sp += 1;
                if cond == 0 {
                    // Skip to matching ELSE or END.
                    pc = skip_to_else_or_end(code, pc);
                }
            }

            OP_ELSE => {
                // Skip to matching END.
                pc = skip_to_end(code, pc);
            }

            OP_BR | OP_BR_IF => {
                let (depth, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let do_br = if op == OP_BR_IF {
                    s.pop().ok_or(Trap::StackUnderflow)?.as_i32() != 0
                } else {
                    true
                };
                if do_br {
                    let target = depth as usize;
                    if target < s.label_sp {
                        let target_idx = s.label_sp - 1 - target;
                        pc = s.label_pc[target_idx];
                    }
                }
            }

            OP_RETURN => return Ok(()),

            OP_CALL => {
                let (func_idx, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                // Imports are function indices 0..N. Treat all calls as WASI imports for now.
                let errno = dispatch_import_by_idx(func_idx, caps, &mut s);
                if errno == 76 {
                    return Err(Trap::CapabilityDenied);
                }
            }

            OP_DROP => {
                s.pop().ok_or(Trap::StackUnderflow)?;
            }

            OP_SELECT => {
                let cond = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                let b = s.pop().ok_or(Trap::StackUnderflow)?;
                let a = s.pop().ok_or(Trap::StackUnderflow)?;
                s.push(if cond != 0 { a } else { b });
            }

            OP_LOCAL_GET => {
                let (idx, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let v = if (idx as usize) < MAX_LOCALS {
                    s.locals[idx as usize]
                } else {
                    Val::I32(0)
                };
                if !s.push(v) {
                    return Err(Trap::StackOverflow);
                }
            }

            OP_LOCAL_SET => {
                let (idx, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let v = s.pop().ok_or(Trap::StackUnderflow)?;
                if (idx as usize) < MAX_LOCALS {
                    s.locals[idx as usize] = v;
                }
            }

            OP_LOCAL_TEE => {
                let (idx, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let v = s.top().ok_or(Trap::StackUnderflow)?;
                if (idx as usize) < MAX_LOCALS {
                    s.locals[idx as usize] = v;
                }
            }

            OP_I32_LOAD => {
                let _align = code[pc];
                pc += 1;
                let (offset, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let base = s.pop().ok_or(Trap::StackUnderflow)?.as_i32() as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(Trap::MemoryOob);
                }
                let v = i32::from_le_bytes([
                    memory[addr],
                    memory[addr + 1],
                    memory[addr + 2],
                    memory[addr + 3],
                ]);
                if !s.push(Val::I32(v)) {
                    return Err(Trap::StackOverflow);
                }
            }

            OP_I32_STORE => {
                let _align = code[pc];
                pc += 1;
                let (offset, consumed) = leb128_u32(&code[pc..]);
                pc += consumed;
                let v = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                let base = s.pop().ok_or(Trap::StackUnderflow)?.as_i32() as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(Trap::MemoryOob);
                }
                let b = v.to_le_bytes();
                memory[addr..addr + 4].copy_from_slice(&b);
            }

            OP_I32_CONST => {
                let (v, consumed) = leb128_i32(&code[pc..]);
                pc += consumed;
                if !s.push(Val::I32(v)) {
                    return Err(Trap::StackOverflow);
                }
            }

            OP_I64_CONST => {
                let (v, consumed) = leb128_i64(&code[pc..]);
                pc += consumed;
                if !s.push(Val::I64(v)) {
                    return Err(Trap::StackOverflow);
                }
            }

            OP_I32_EQZ => {
                let a = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                if !s.push(Val::I32(if a == 0 { 1 } else { 0 })) {
                    return Err(Trap::StackOverflow);
                }
            }

            OP_I32_EQ => {
                let (b, a) = pop2(&mut s)?;
                s.push(Val::I32((a == b) as i32));
            }
            OP_I32_NE => {
                let (b, a) = pop2(&mut s)?;
                s.push(Val::I32((a != b) as i32));
            }
            OP_I32_LT_S => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32((a < b) as i32));
            }
            OP_I32_LT_U => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a < b) as i32));
            }
            OP_I32_GT_S => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32((a > b) as i32));
            }
            OP_I32_GT_U => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a > b) as i32));
            }
            OP_I32_LE_S => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32((a <= b) as i32));
            }
            OP_I32_LE_U => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a <= b) as i32));
            }
            OP_I32_GE_S => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32((a >= b) as i32));
            }
            OP_I32_GE_U => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a >= b) as i32));
            }

            OP_I32_CLZ => {
                let a = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                s.push(Val::I32(a.leading_zeros() as i32));
            }
            OP_I32_CTZ => {
                let a = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                s.push(Val::I32(a.trailing_zeros() as i32));
            }
            OP_I32_POPCNT => {
                let a = s.pop().ok_or(Trap::StackUnderflow)?.as_i32();
                s.push(Val::I32(a.count_ones() as i32));
            }

            OP_I32_ADD => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a.wrapping_add(b)));
            }
            OP_I32_SUB => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a.wrapping_sub(b)));
            }
            OP_I32_MUL => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a.wrapping_mul(b)));
            }
            OP_I32_DIV_S => {
                let (b, a) = pop2_i32(&mut s)?;
                if b == 0 {
                    return Err(Trap::DivisionByZero);
                }
                s.push(Val::I32(a.wrapping_div(b)));
            }
            OP_I32_DIV_U => {
                let (b, a) = pop2_u32(&mut s)?;
                if b == 0 {
                    return Err(Trap::DivisionByZero);
                }
                s.push(Val::I32((a / b) as i32));
            }
            OP_I32_REM_S => {
                let (b, a) = pop2_i32(&mut s)?;
                if b == 0 {
                    return Err(Trap::DivisionByZero);
                }
                s.push(Val::I32(a.wrapping_rem(b)));
            }
            OP_I32_REM_U => {
                let (b, a) = pop2_u32(&mut s)?;
                if b == 0 {
                    return Err(Trap::DivisionByZero);
                }
                s.push(Val::I32((a % b) as i32));
            }
            OP_I32_AND => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a & b));
            }
            OP_I32_OR => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a | b));
            }
            OP_I32_XOR => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a ^ b));
            }
            OP_I32_SHL => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a << (b & 31)) as i32));
            }
            OP_I32_SHR_S => {
                let (b, a) = pop2_i32(&mut s)?;
                s.push(Val::I32(a >> (b & 31)));
            }
            OP_I32_SHR_U => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32((a >> (b & 31)) as i32));
            }
            OP_I32_ROTL => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32(a.rotate_left(b & 31) as i32));
            }
            OP_I32_ROTR => {
                let (b, a) = pop2_u32(&mut s)?;
                s.push(Val::I32(a.rotate_right(b & 31) as i32));
            }

            _ => return Err(Trap::UnsupportedOpcode),
        }
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn pop2(s: &mut InterpState) -> Result<(Val, Val), Trap> {
    let b = s.pop().ok_or(Trap::StackUnderflow)?;
    let a = s.pop().ok_or(Trap::StackUnderflow)?;
    Ok((b, a))
}
fn pop2_i32(s: &mut InterpState) -> Result<(i32, i32), Trap> {
    let (b, a) = pop2(s)?;
    Ok((b.as_i32(), a.as_i32()))
}
fn pop2_u32(s: &mut InterpState) -> Result<(u32, u32), Trap> {
    let (b, a) = pop2_i32(s)?;
    Ok((b as u32, a as u32))
}

fn leb128_u32(bytes: &[u8]) -> (u32, usize) {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
        if shift >= 35 {
            break;
        }
    }
    (0, 1)
}

fn leb128_i32(bytes: &[u8]) -> (i32, usize) {
    let mut result: i32 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7F) as i32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 32 && (byte & 0x40) != 0 {
                result |= !0 << shift;
            }
            return (result, i + 1);
        }
        if shift >= 35 {
            break;
        }
    }
    (0, 1)
}

fn leb128_i64(bytes: &[u8]) -> (i64, usize) {
    let mut result: i64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7F) as i64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && (byte & 0x40) != 0 {
                result |= !0 << shift;
            }
            return (result, i + 1);
        }
        if shift >= 70 {
            break;
        }
    }
    (0, 1)
}

fn skip_to_end(code: &[u8], pc: usize) -> usize {
    let mut depth = 1usize;
    let mut i = pc;
    while i < code.len() {
        match code[i] {
            OP_BLOCK | OP_LOOP | OP_IF => {
                depth += 1;
                i += 1;
            }
            OP_END => {
                i += 1;
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    code.len()
}

fn skip_to_else_or_end(code: &[u8], pc: usize) -> usize {
    let mut depth = 1usize;
    let mut i = pc;
    while i < code.len() {
        match code[i] {
            OP_BLOCK | OP_LOOP | OP_IF => {
                depth += 1;
                i += 1;
            }
            OP_ELSE => {
                if depth == 1 {
                    return i + 1;
                }
                i += 1;
            }
            OP_END => {
                i += 1;
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    code.len()
}

/// Dispatch a WASI import by function index.
fn dispatch_import_by_idx(func_idx: u32, caps: u64, _s: &mut InterpState) -> u32 {
    // For now, all non-zero indices that are beyond imported functions are
    // module-internal calls — treat as OK. Index 0..N_IMPORTS are WASI.
    // The real implementation maps function indices to import names via the
    // module's function section; for now use a fixed table.
    match func_idx {
        0 if caps & CAP_STDOUT == 0 => 76,  // fd_write
        1 if caps & CAP_STDIN == 0 => 76,   // fd_read
        2 if caps & CAP_FS_READ == 0 => 76, // path_open
        3 | 4 if caps & CAP_NET == 0 => 76, // sock_send/sock_recv
        _ => 0,                             // internal call — OK
    }
}

/// Dispatch a WASI import call by name (called from module init/linker path).
/// Returns the WASI errno (0 = success, 76 = ENOTCAPABLE).
pub fn dispatch_import(module_caps: u64, import_name: &[u8], _args: &[u64]) -> u32 {
    match import_name {
        b"fd_write" if module_caps & CAP_STDOUT == 0 => 76,
        b"fd_read" if module_caps & CAP_STDIN == 0 => 76,
        b"path_open" if module_caps & CAP_FS_READ == 0 => 76,
        b"path_create_file" if module_caps & CAP_FS_WRITE == 0 => 76,
        b"sock_send" | b"sock_recv" if module_caps & CAP_NET == 0 => 76,
        b"fd_write" | b"fd_read" | b"path_open" | b"path_create_file" | b"sock_send"
        | b"sock_recv" => 0,
        _ => 76, // unknown import — deny
    }
}
