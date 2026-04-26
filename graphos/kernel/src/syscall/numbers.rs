// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Syscall number constants.
//!
//! These are the ABI contract between userspace and the kernel.
//! Numbers are stable once assigned — never reorder or reassign.
//! New syscalls are appended at the end of each range.
//!
//! Range 0x000–0x0FF: Core process lifecycle.
//! Range 0x100–0x1FF: I/O and IPC.
//! Range 0x200–0x2FF: Memory management.
//! Range 0x300–0x3FF: Graph operations (GraphOS-specific).

// ---- Process lifecycle (0x000) ----

/// Terminate the calling task. arg0 = exit code.
pub const SYS_EXIT: u64 = 0x001;

/// Voluntarily yield the CPU to the scheduler.
pub const SYS_YIELD: u64 = 0x002;

/// Spawn a new task. arg0 = request ptr, arg1 = entry point.
/// For ring-3 callers, arg0 may be a well-known service name or an absolute
/// bootfs path such as `/boot/services/graphd.elf`.
/// Returns the new task's TaskId, or u64::MAX on error.
pub const SYS_SPAWN: u64 = 0x003;

/// Spawn a new thread in the calling task's address space.
/// arg0 = entry function pointer (ring-3 address).
/// arg1 = argument passed in rdi when the thread enters ring-3.
/// arg2 = user-stack top (pre-allocated by caller via SYS_MMAP).
/// Returns the new thread's TaskId, or u64::MAX on error.
pub const SYS_THREAD_SPAWN: u64 = 0x010;

/// Block until the thread with the given TaskId has exited.
/// arg0 = TaskId returned by SYS_THREAD_SPAWN.
/// Returns 0 on success, u64::MAX if the TaskId is invalid.
pub const SYS_THREAD_JOIN: u64 = 0x011;

/// Exit the calling thread (equivalent to SYS_EXIT for threads).
/// arg0 = exit code (currently ignored).
pub const SYS_THREAD_EXIT: u64 = 0x012;

// ---- I/O and IPC (0x100) ----

/// Write bytes. arg0 = fd, arg1 = buffer ptr, arg2 = length.
/// Returns number of bytes written, or u64::MAX on error.
pub const SYS_WRITE: u64 = 0x100;

/// Create an IPC channel. arg0 = max message size in bytes.
/// Returns the channel ID, or u64::MAX on error.
pub const SYS_CHANNEL_CREATE: u64 = 0x101;

/// Send a message on a channel. arg0 = channel ID, arg1 = buffer ptr,
/// arg2 = length, arg3 = MsgTag (u8). Returns 0 on success, u64::MAX on error.
pub const SYS_CHANNEL_SEND: u64 = 0x102;

/// Receive a message from a channel. arg0 = channel ID, arg1 = buffer ptr,
/// arg2 = buffer capacity. Returns payload bytes received in low 16 bits,
/// MsgTag in bits [16..24], reply endpoint in bits [24..56]. Returns 0 if empty,
/// u64::MAX on error.
pub const SYS_CHANNEL_RECV: u64 = 0x103;

/// Open a VFS path. arg0 = path ptr.
/// Returns an fd, or u64::MAX on error.
pub const SYS_VFS_OPEN: u64 = 0x104;

/// Read bytes from an fd. arg0 = fd, arg1 = buffer ptr, arg2 = capacity.
/// Returns bytes read, 0 on EOF, or u64::MAX on error.
pub const SYS_VFS_READ: u64 = 0x105;

/// Close an fd. arg0 = fd.
/// Returns 0 on success, or u64::MAX on error.
pub const SYS_VFS_CLOSE: u64 = 0x106;

/// Write bytes to an open fd (writable filesystem).
/// arg0 = fd, arg1 = buffer ptr, arg2 = length.
/// Returns bytes written, or u64::MAX on error.
pub const SYS_VFS_WRITE: u64 = 0x107;

/// Create a file (if it does not exist) and open it.
/// arg0 = path ptr (null-terminated, max 127 bytes).
/// Returns an fd, or u64::MAX on error.
pub const SYS_VFS_CREATE: u64 = 0x108;

/// Create a directory at the given path.
/// arg0 = path ptr (null-terminated, max 127 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_VFS_MKDIR: u64 = 0x10B;

/// Remove a file or empty directory.
/// arg0 = path ptr (null-terminated, max 127 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_VFS_UNLINK: u64 = 0x10C;

/// Mount a filesystem at a path prefix.  Capability-gated (root only).
/// arg0 = mount-point path ptr (null-terminated), arg1 = fs type
///   (0 = ramfs, 1 = ext2, 2 = fat32), arg2 = reserved (pass 0).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_MOUNT: u64 = 0x109;

/// Unmount the filesystem at a path prefix.  Capability-gated (root only).
/// arg0 = mount-point path ptr (null-terminated).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_UMOUNT: u64 = 0x10A;

/// Create a socket and write UUID handle bytes to caller buffer.
/// arg0 = out ptr, arg1 = out len (>=16).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_SOCKET: u64 = 0x120;

/// Bind a socket to a local port.
/// arg0 = socket handle ptr, arg1 = local port (u16).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_BIND: u64 = 0x121;

/// Connect a socket to remote endpoint.
/// arg0 = socket handle ptr, arg1 = remote IPv4 (u32), arg2 = remote port (u16).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_CONNECT: u64 = 0x122;

/// Send payload on socket.
/// arg0 = socket handle ptr, arg1 = payload ptr, arg2 = payload len.
/// Returns bytes sent on success, u64::MAX on error.
pub const SYS_SEND: u64 = 0x123;

/// Receive payload from socket.
/// arg0 = socket handle ptr, arg1 = out ptr, arg2 = out len.
/// Returns bytes received on success, u64::MAX on error.
pub const SYS_RECV: u64 = 0x124;

/// Close socket by UUID handle.
/// arg0 = socket handle ptr.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_CLOSE_SOCK: u64 = 0x125;

/// Put a bound TCP socket into passive-listen mode.
/// arg0 = socket handle ptr.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_LISTEN: u64 = 0x126;

/// Accept one pending inbound connection on a listening socket.
/// arg0 = listen socket handle ptr, arg1 = out ptr (16 bytes) for accepted socket UUID.
/// Returns (remote_ip << 16 | remote_port) on success, u64::MAX on error.
pub const SYS_ACCEPT: u64 = 0x127;

/// Read aggregate network link and packet counters.
/// No arguments.
/// Returns packed u64: bit63=link_ready, bits0..31=tx_packets, bits32..62=rx_packets.
pub const SYS_NET_STATS: u64 = 0x128;

/// Get effective UID for current task.
pub const SYS_GETUID: u64 = 0x130;

/// Get effective GID for current task.
pub const SYS_GETGID: u64 = 0x131;

/// Set UID/GID for current task (root only).
/// arg0 = uid, arg1 = gid.
pub const SYS_SETUID: u64 = 0x132;

/// Login and attach a new session.
/// arg0 = username ptr, arg1 = password ptr.
pub const SYS_LOGIN: u64 = 0x133;

/// Logout current session.
pub const SYS_LOGOUT: u64 = 0x134;

/// Allocate a pseudo-terminal for the current session.
/// No arguments.
/// Returns the tty index (u32) on success, u64::MAX on error.
pub const SYS_PTY_ALLOC: u64 = 0x135;

/// Attach the current task to an existing session by UUID.
/// arg0 = session UUID ptr (16 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_SESSION_ATTACH: u64 = 0x136;

/// Write bytes to a PTY's input ring (bytes going into the shell).
/// arg0 = tty index, arg1 = buf ptr, arg2 = len.
/// Returns bytes written, u64::MAX on error.
pub const SYS_PTY_WRITE: u64 = 0x137;

/// Read bytes from a PTY's output ring (bytes coming from the shell).
/// arg0 = tty index, arg1 = buf ptr, arg2 = len.
/// Returns bytes read (0 if empty), u64::MAX on error.
pub const SYS_PTY_READ: u64 = 0x138;

/// Service heartbeat — signals the kernel watchdog that the calling
/// service is still alive.  No arguments.
/// Returns 0 on success, u64::MAX if the caller is not a tracked service.
pub const SYS_HEARTBEAT: u64 = 0x150;

// ---- Performance sampling (0x160) ----

/// Sample PMU counters for the current task (or another task if root).
/// arg0 = task UUID ptr (16 bytes); pass all-zeros for the calling task.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_PERF_SAMPLE: u64 = 0x160;

/// Read the latest PMU sample for a task.
/// arg0 = task UUID ptr (16 bytes), arg1 = out buf ptr, arg2 = out buf len.
/// Writes a serialised `PerfSample` record into the buffer.
/// Returns bytes written, or u64::MAX on error.
pub const SYS_PERF_READ: u64 = 0x161;

/// Drain pending kernel security audit records into caller buffer.
/// Capability-gated: only `protected_strict` (system service) tasks may call this.
/// arg0 = buffer ptr, arg1 = buffer length in bytes (must be a multiple of 56).
/// Returns packed u64: low 32 bits = records written, high 32 bits = dropped count.
/// Returns u64::MAX on permission error.
pub const SYS_AUDIT_READ: u64 = 0x162;

/// Fill a caller buffer with cryptographically strong random bytes.
/// arg0 = buffer ptr, arg1 = buffer length.
/// Returns bytes written, or u64::MAX on error.
pub const SYS_GETRANDOM: u64 = 0x163;

// ---- Wi-Fi (0x170) ----

/// Trigger an 802.11 passive scan.  No arguments.
/// Returns 1 if scan started, 0 if hardware not ready, u64::MAX on error.
pub const SYS_WIFI_SCAN: u64 = 0x170;

/// Associate with a BSS using WPA2-PSK or WPA3-SAE.
/// arg0 = SSID ptr (null-terminated, max 32 bytes).
/// arg1 = passphrase ptr (null-terminated, max 63 bytes).
/// Returns 0 if association initiated, u64::MAX on error.
pub const SYS_WIFI_CONNECT: u64 = 0x171;

/// Return current Wi-Fi association state.
/// No arguments.  Returns 0=Idle, 1=Scanning, 2=Associating, 3=Associated,
/// 4=Failed.
pub const SYS_WIFI_STATE: u64 = 0x172;

// ---- Bluetooth (0x178) ----

/// Trigger a BT device scan.  No arguments.
/// Returns 1 if scan started, u64::MAX on error.
pub const SYS_BT_SCAN: u64 = 0x178;

/// Open an L2CAP connection to a device.
/// arg0 = ACL handle (u16), arg1 = PSM (u16).
/// Returns the local CID on success, u64::MAX on error.
pub const SYS_BT_CONNECT: u64 = 0x179;

/// Send a payload on an open L2CAP channel.
/// arg0 = local CID (u16), arg1 = payload ptr, arg2 = payload len.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_BT_SEND: u64 = 0x17A;

/// Close an L2CAP channel.
/// arg0 = local CID (u16).
/// Returns 0.
pub const SYS_BT_CLOSE: u64 = 0x17B;

/// Probe for an installed driver for a device UUID.
/// arg0 = device UUID ptr (16 bytes), arg1 = optional out-driver UUID ptr (16 bytes).
/// Returns 1 if available, 0 if none, u64::MAX on error.
pub const SYS_DRIVER_PROBE: u64 = 0x140;

/// Install/register a driver package for a device UUID.
/// arg0 = package UUID ptr (16 bytes), arg1 = device UUID ptr (16 bytes).
/// arg2 = packed manifest ptr/len (low 32 bits = ptr, high 32 bits = len), arg3 = signature ptr (64 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_DRIVER_INSTALL: u64 = 0x141;

// ---- Memory (0x200) ----

/// Map memory into the current user address space.
/// arg0 = path ptr (0 for anonymous), arg1 = length, arg2 = prot flags,
/// arg3 = map flags, arg4 = file offset, arg5 = reserved.
/// Returns the mapped user virtual address, or u64::MAX on error.
pub const SYS_MMAP: u64 = 0x200;

/// Unmap a previous mapping from the current user address space.
/// arg0 = address, arg1 = length.
/// Returns 0 on success, or u64::MAX on error.
pub const SYS_MUNMAP: u64 = 0x201;

/// Sleep for at least N ticks (1 tick ≈ 1 ms). arg0 = tick count.
/// Returns 0 when woken. Actual latency is 1–2 ms due to PIT granularity.
pub const SYS_SLEEP: u64 = 0x202;

// ---- Power management (0x2F0) ----
/// Soft power-off (ACPI S5). Capability-gated (CAP_POWEROFF required).
pub const SYS_POWEROFF: u64 = 0x2F0;
/// System reboot. Capability-gated.
pub const SYS_REBOOT: u64 = 0x2F1;
/// Suspend to RAM (ACPI S3). Capability-gated.
pub const SYS_SUSPEND: u64 = 0x2F2;

// ---- Window / Surface (0x400) ----

/// Allocate a shared pixel surface. arg0 = width (u16 in low 16 bits),
/// arg1 = height (u16 in low 16 bits).
/// Returns packed u64: bits [0..32] = surface_id, bits [32..64] = low 32 bits
/// of the user virtual address where the pixel buffer is mapped.
/// Returns u64::MAX on error.
pub const SYS_SURFACE_CREATE: u64 = 0x400;

/// Mark a surface as ready to present to the compositor.
/// arg0 = surface_id. Validates that the caller owns the surface.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_SURFACE_PRESENT: u64 = 0x401;

/// Destroy a surface and return its frames to the allocator.
/// arg0 = surface_id. Validates that the caller owns the surface.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_SURFACE_DESTROY: u64 = 0x402;

/// Query whether the compositor's present queue is non-empty.
/// Returns 1 if at least one surface is pending, 0 otherwise.
pub const SYS_SURFACE_QUERY_PENDING: u64 = 0x403;

/// Request the kernel desktop compositor to flush the pending surface queue.
/// Only the registered ring-3 compositor service may call this.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_SURFACE_FLUSH: u64 = 0x404;

/// Phase J GPU compositor commit — mark a surface ready with spring-physics routing.
/// Equivalent to SYS_SURFACE_PRESENT but routes through the GpuCompositor.
/// arg0 = surface_id.
/// Returns the monotonic commit counter value on success, u64::MAX on error.
pub const SYS_SURFACE_COMMIT: u64 = 0x405;

/// Toggle Exposé (all-windows overview) mode in the GPU compositor.
/// arg0 = window_w (u32, canonical window width for layout), arg1 = window_h (u32).
/// Returns 1 if Exposé is now active, 0 if now inactive.
pub const SYS_EXPOSE_TOGGLE: u64 = 0x406;

/// Query spring-physics transform for a surface.
/// arg0 = surface_id, arg1 = out_ptr (4×i32: x, y, scale_fp1000, opacity_fp1000).
/// Returns 0 on success, u64::MAX if surface not found.
pub const SYS_SURFACE_TRANSFORM: u64 = 0x407;

// ---- UI / theme (0x420) ----

/// Set the active UI theme.
/// arg0 = ThemeId: 0 = DarkGlass, 1 = LightFrost, 2 = HighContrast.
/// Returns 0 on success, 1 if theme_id is invalid.
pub const SYS_THEME_SET: u64 = 0x420;

/// Get the active UI theme ID.
/// Returns ThemeId as u64.
pub const SYS_THEME_GET: u64 = 0x421;

// ---- GPU resource management (0x500–0x50F) ----
// All GPU syscalls require the caller to be the registered compositor service.
// The kernel validates the calling task's identity before forwarding to the
// virtio-gpu driver.  Non-compositor callers receive u64::MAX (EPERM).

/// Query GPU capabilities.
/// Returns a packed capability word:
///   bits [0..0]  = native_gpu_submit_supported (1 = yes)
///   bits [1..1]  = multi_planar_supported
///   bits [16..31] = max_texture_dimension (log2, e.g. 13 = 8192 px)
///   bits [32..47] = max_command_buffer_bytes (in units of 64 bytes)
pub const SYS_GPU_QUERY_CAPS: u64 = 0x500;

/// Create a GPU texture resource.
/// arg0 = width  (u32)
/// arg1 = height (u32)
/// arg2 = format (u8): 0=BGRA8, 1=RGBA16F, 2=R8, 3=RG8
/// Returns resource_id (u32), or u64::MAX on error.
pub const SYS_GPU_RESOURCE_CREATE: u64 = 0x501;

/// Destroy a GPU texture resource previously created with SYS_GPU_RESOURCE_CREATE.
/// arg0 = resource_id (u32)
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GPU_RESOURCE_DESTROY: u64 = 0x502;

/// Submit a deprecated legacy 3-D command buffer entrypoint.
/// arg0 = ptr to command buffer (user address, must be readable)
/// arg1 = byte length of command buffer
/// GraphOS-native submitters must use `SYS_GPU_SUBMIT` instead.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GPU_SUBMIT_3D: u64 = 0x503;

/// Import a surface backing page as a GPU texture resource.
/// arg0 = surface_id (u32) — must be owned by or shared with the calling compositor
/// arg1 = resource_id (u32) — existing GPU resource to bind the surface into
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GPU_SURFACE_IMPORT: u64 = 0x504;

/// Allocate a GPU timeline fence.
/// No arguments.
/// Returns fence_id (u32), or u64::MAX on error.
pub const SYS_GPU_FENCE_ALLOC: u64 = 0x505;

/// Block until a GPU fence is signalled (or timeout in scheduler ticks).
/// arg0 = fence_id (u32)
/// arg1 = timeout_ticks (u64); u64::MAX = wait forever.
/// Returns 0 on signal, 1 on timeout, u64::MAX on error.
pub const SYS_GPU_FENCE_WAIT: u64 = 0x506;

/// Non-blocking poll of a GPU fence.
/// arg0 = fence_id (u32).
/// Returns 1 if signalled, 0 if pending.
pub const SYS_GPU_FENCE_POLL: u64 = 0x507;

/// Submit a GraphOS-native GPU command buffer.
/// arg0 = ptr to encoded wire buffer (user address, readable)
/// arg1 = byte length of buffer (max 512 KiB)
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GPU_SUBMIT: u64 = 0x508;

/// Claim runtime scanout ownership for the ring-3 compositor and register the
/// supplied surface as the fullscreen desktop background.
/// arg0 = surface_id (u32) owned by the compositor task.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_COMPOSITOR_CLAIM_DISPLAY: u64 = 0x509;

// ---- Input (0x410) ----

/// Set keyboard focus to the calling task on a given IPC channel.
/// arg0 = channel id (u32). Passing 0 releases focus.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_INPUT_SET_FOCUS: u64 = 0x410;

/// Register the calling task's window rectangle for pointer hit-testing.
/// arg0 = packed: x (i16) | y (i16) << 16 (i32 coords).
/// arg1 = packed: w (u16) | h (u16) << 16 (dimensions).
/// arg2 = IPC channel to receive pointer events.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_INPUT_REGISTER_WINDOW: u64 = 0x411;

/// Unregister the calling task's window rectangle.
/// Returns 0.
pub const SYS_INPUT_UNREGISTER_WINDOW: u64 = 0x412;

/// Subscribe the calling task's IPC channel to display-system frame-tick broadcasts.
/// arg0 = channel_id (u32) — the channel that will receive FrameTick messages.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_FRAME_TICK_SUBSCRIBE: u64 = 0x413;

// ---- Graph (0x300) ----

/// Add a node to the graph.
/// arg0 = NodeKind (u16), arg1 = flags (u32), arg2 = creator NodeId.
/// Returns the assigned NodeId, or u64::MAX on error.
pub const SYS_GRAPH_ADD_NODE: u64 = 0x300;

/// Add a weighted edge to the graph.
/// arg0 = from NodeId, arg1 = to NodeId, arg2 = EdgeKind (u16),
/// arg3 = flags (u32), arg4 = weight (16.16 fixed-point).
/// Returns the assigned EdgeId, or u64::MAX on error.
pub const SYS_GRAPH_ADD_EDGE: u64 = 0x301;

/// Query whether a node exists.
/// arg0 = NodeId.
/// Returns 1 if exists, 0 if not.
pub const SYS_GRAPH_NODE_EXISTS: u64 = 0x302;

/// Get node kind.
/// arg0 = NodeId.
/// Returns NodeKind as u64, or u64::MAX if not found.
pub const SYS_GRAPH_NODE_KIND: u64 = 0x303;

/// Get graph statistics.
/// No arguments.
/// Returns packed: low 32 bits = node_count, high 32 bits = edge_count.
pub const SYS_GRAPH_STATS: u64 = 0x304;

/// Get current arena generation (monotonic mutation counter).
/// No arguments.
/// Returns the generation as u64.
pub const SYS_GRAPH_GENERATION: u64 = 0x305;

/// Look up a bootstrap service binding by name.
/// arg0 = service name ptr.
/// Returns packed: bits [0..48] = graph node id, bits [48..64] = stable service id.
/// Returns u64::MAX if the service is unknown.
pub const SYS_GRAPH_SERVICE_LOOKUP: u64 = 0x306;

/// Look up a bootstrap service binding by name and write stable UUID bytes.
/// arg0 = service name ptr, arg1 = uuid_out ptr, arg2 = uuid_out len.
/// If arg1 != 0 and arg2 >= 16, writes 16 UUID bytes to uuid_out.
/// Returns packed: bits [0..48] = graph node id, bits [48..64] = stable service id.
/// Returns u64::MAX if the service is unknown or output buffer is invalid.
pub const SYS_GRAPH_SERVICE_LOOKUP_UUID: u64 = 0x307;

/// Registry lookup by service name.
/// arg0 = service name ptr, arg1 = out ptr (optional), arg2 = out len.
/// If out ptr/len are provided, kernel writes UUID/channel/task metadata.
/// Returns channel alias (u32 in low bits) or u64::MAX on error.
pub const SYS_REGISTRY_LOOKUP: u64 = 0x308;

/// Registry register/update for dynamic services.
/// arg0 = service name ptr, arg1 = channel alias (u32).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_REGISTRY_REGISTER: u64 = 0x309;

/// Registry subscription generation poll.
/// arg0 = caller's last seen generation.
/// Returns current generation if changed, else 0.
pub const SYS_REGISTRY_SUBSCRIBE: u64 = 0x30A;

/// IPC capability delegation.
/// arg0 = target TaskId, arg1 = channel alias (u32), arg2 = capability bits.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_IPC_CAP_GRANT: u64 = 0x30B;

/// IPC capability revocation.
/// arg0 = target TaskId, arg1 = channel alias (u32), arg2 = capability bits.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_IPC_CAP_REVOKE: u64 = 0x30C;

/// Trigger one EM M-step to update per-type-pair walk parameters.
/// No arguments.  Returns the new epoch number (u64), or u64::MAX on error.
/// Callable only by uid == 0 tasks.
pub const SYS_GRAPH_EM_STEP: u64 = 0x30D;

/// Read EM calibration statistics for one type pair.
/// arg0 = src_kind (u16), arg1 = dst_kind (u16).
/// Returns packed: [0..32]=transitions, [32..64]=epoch, or u64::MAX on error.
pub const SYS_GRAPH_EM_STATS: u64 = 0x30E;

// ---- Cognitive (0x380) ----

/// Index a document into the cognitive subsystem.
/// arg0 = doc_text ptr, arg1 = doc_text len, arg2 = doc_type (u8),
/// arg3 = creator NodeId.
/// Returns packed: low 32 bits = chunk_count, high 32 bits = terms_indexed.
/// Returns u64::MAX on error.
pub const SYS_COGNITIVE_INDEX: u64 = 0x380;

/// Run cognitive retrieval pipeline.
/// arg0 = query ptr, arg1 = query len, arg2 = query fingerprint (u64).
/// Returns packed: low 8 bits = phase_reached, bits [8..16] = evidence_count,
/// bits [16..32] = confidence (16.16 truncated to u16), bits [32..40] = strategy.
/// Returns u64::MAX on error.
pub const SYS_COGNITIVE_QUERY: u64 = 0x381;

/// Redact secrets from a buffer.
/// arg0 = input ptr, arg1 = input len, arg2 = output ptr, arg3 = output len.
/// Returns packed: low 32 bits = output_len, high 32 bits = redaction_count.
/// Returns u64::MAX on error.
pub const SYS_COGNITIVE_REDACT: u64 = 0x382;

// ---- Grid computing (0x500) ----
// All grid syscalls require CAP_GRID capability.

/// List grid peers into a caller buffer.
/// arg0 = out_ptr (array of GridPeerInfo structs), arg1 = max_count.
/// Returns peer count written, or u64::MAX on error.
pub const SYS_GRID_LIST_PEERS: u64 = 0x500;

/// Spawn a task on the best-fit remote peer node.
/// arg0 = task_binary_uuid ptr (16 bytes), arg1 = entry arg,
/// arg2 = required_caps (u8), arg3 = requester task index.
/// Returns correlation UUID low 64 bits, or u64::MAX on failure (run locally).
pub const SYS_GRID_SPAWN: u64 = 0x501;

/// Allocate pages from a remote peer's free RAM.
/// arg0 = pages (u32), arg1 = out_ptr for (node_uuid, phys_base, granted) triple.
/// Returns pages_granted, or 0 on failure.
pub const SYS_GRID_ALLOC: u64 = 0x502;

/// Mount a remote peer's VFS subtree under `/grid/<node-uuid>/`.
/// arg0 = node_uuid ptr (16 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GRID_MOUNT: u64 = 0x503;

/// Unmount a remote grid VFS.
/// arg0 = node_uuid ptr (16 bytes).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GRID_UMOUNT: u64 = 0x504;

/// Forward an IPC message to a remote channel UUID.
/// arg0 = channel_uuid ptr (16 bytes), arg1 = payload ptr, arg2 = payload len.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GRID_IPC_SEND: u64 = 0x505;

/// Submit a surface to a remote GPU (grid GPU sharing).
/// arg0 = peer_node_uuid ptr (16 bytes), arg1 = surface_id.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GRID_GPU_SURFACE: u64 = 0x506;

/// Get local node UUID (16 bytes written to arg0).
/// Returns 0 on success, u64::MAX on error.
pub const SYS_GRID_NODE_UUID: u64 = 0x507;

// ---- OTA update (0x510) ----

/// Fetch an OTA update bundle via HTTP GET and stage it for the next boot.
/// arg0 = URL ptr (ASCII, null-terminated or length-prefixed via arg1).
/// arg1 = URL length (bytes, not including any null terminator).
/// The kernel fetches the bundle, verifies the ed25519 signature using the
/// TPM-enrolled key, and stages it to the inactive A/B boot slot.
/// Returns 0 on success, u64::MAX on error.
pub const SYS_FETCH_UPDATE: u64 = 0x510;

// ---- TLS (0x511) ----

/// Signal the kernel that the ring-3 TLS service has completed a validated
/// handshake and the TLS transport layer is ready for use.
///
/// Capability-gated: only `protected_strict` tasks (uid=0) may call this.
/// After this returns successfully, `SYS_FETCH_UPDATE` will proceed.
///
/// No arguments.
/// Returns 0 on success, u64::MAX on permission error.
pub const SYS_TLS_SET_AVAILABLE: u64 = 0x511;

/// Signal the kernel that the TLS service has become unavailable (e.g. after a crash).
/// Capability-gated: only `protected_strict` tasks (uid=0) may call this.
/// Returns 0 on success.
pub const SYS_TLS_SET_UNAVAILABLE: u64 = 0x512;

/// Query TLS availability.
/// No arguments.
/// Returns 1 if available, 0 if not.
pub const SYS_TLS_AVAILABLE: u64 = 0x513;
