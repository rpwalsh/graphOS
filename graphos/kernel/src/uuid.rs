// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! UUID primitives for GraphOS identity.
//!
//! This module provides a compact `Uuid128` type with deterministic
//! name-based generation used for stable service identity.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Uuid128 {
    hi: u64,
    lo: u64,
}

impl Uuid128 {
    pub const NIL: Self = Self { hi: 0, lo: 0 };

    pub const fn from_u64_pair(hi: u64, lo: u64) -> Self {
        Self { hi, lo }
    }

    pub const fn to_u64_pair(self) -> (u64, u64) {
        (self.hi, self.lo)
    }

    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.hi.to_be_bytes());
        out[8..].copy_from_slice(&self.lo.to_be_bytes());
        out
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        let mut hi = [0u8; 8];
        let mut lo = [0u8; 8];
        hi.copy_from_slice(&bytes[..8]);
        lo.copy_from_slice(&bytes[8..]);
        Self {
            hi: u64::from_be_bytes(hi),
            lo: u64::from_be_bytes(lo),
        }
    }

    /// Runtime UUID v4 using RDRAND on x86_64.
    ///
    /// Returns `None` if hardware entropy is unavailable.
    pub fn v4_random() -> Option<Self> {
        if !has_rdrand() {
            return None;
        }

        let hi = rdrand_u64()?;
        let lo = rdrand_u64()?;
        let mut bytes = Self::from_u64_pair(hi, lo).to_bytes();

        // RFC4122 bits.
        bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
        bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10xx

        Some(Self::from_bytes(bytes))
    }

    /// Deterministic v5-style UUID for stable service names.
    ///
    /// We intentionally keep hashing self-contained in kernel (`no_std`, no
    /// external crypto dependency): two seeded FNV-1a passes are combined into
    /// 128 bits, then RFC4122 version/variant bits are applied.
    pub fn v5_service_name(name: &[u8]) -> Self {
        // GraphOS service namespace constant.
        const NS: Uuid128 = Uuid128 {
            hi: 0x6f8d_63a5_26c1_4d71,
            lo: 0x9bb5_42fb_2a4c_5d90,
        };
        Self::v5_with_namespace(NS, name)
    }

    /// Deterministic v5-style UUID under `namespace`.
    pub fn v5_with_namespace(namespace: Self, name: &[u8]) -> Self {
        let (ns_hi, ns_lo) = namespace.to_u64_pair();
        let h1 = fnv1a64(name, ns_hi ^ ns_lo);
        let h2 = fnv1a64(name, ns_hi.rotate_left(13) ^ ns_lo.rotate_right(7));
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&h1.to_be_bytes());
        bytes[8..].copy_from_slice(&h2.to_be_bytes());

        // RFC4122 bits.
        bytes[6] = (bytes[6] & 0x0F) | 0x50; // version 5
        bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10xx

        Self::from_bytes(bytes)
    }
}

/// Namespace UUID for all graph-level identities (UUID v5 base namespace).
pub const UUID_NS_GRAPH: Uuid128 = Uuid128 {
    hi: 0x6f8d_63a5_26c1_4d71,
    lo: 0x9bb5_42fb_2a4c_5d90,
};

/// Convenience generator — replaces `Uuid128::v4_random()` and
/// `Uuid128::v5_with_namespace()` with a unified, discoverable API.
pub struct Uuid128Gen;

impl Uuid128Gen {
    /// Generate a v4 random UUID.  Falls back to a counter-seeded UUID if
    /// RDRAND is not available.
    pub fn v4() -> Uuid128 {
        Uuid128::v4_random().unwrap_or_else(|| {
            static CTR: core::sync::atomic::AtomicU64 =
                core::sync::atomic::AtomicU64::new(0x1234_5678_9abc_def0);
            let hi = CTR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let lo = hi.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            Uuid128::from_u64_pair(hi, lo)
        })
    }

    /// Generate a deterministic v5 UUID under `namespace` from `name`.
    pub fn v5_name(namespace: Uuid128, name: &[u8]) -> Uuid128 {
        Uuid128::v5_with_namespace(namespace, name)
    }
}

/// Type-safe service identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ServiceUuid(pub Uuid128);

impl ServiceUuid {
    pub fn from_service_name(name: &[u8]) -> Self {
        Self(Uuid128::v5_service_name(name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe channel identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ChannelUuid(pub Uuid128);

impl ChannelUuid {
    /// UUID v5 for a well-known service inbox channel, derived from its name.
    /// Stable across reboots; no RDRAND required.
    pub fn from_service_name(name: &[u8]) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x4348_4e2d_494e_424f,
            lo: 0x8a1c_3e5f_b729_d000,
        };
        Self(Uuid128::v5_with_namespace(NS, name))
    }

    /// Alias shim: UUID v5 keyed by the legacy integer channel ID.
    /// Used only at the syscall ABI boundary while integer aliases are still live.
    pub fn from_channel_id(channel_id: u32) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x4950_432d_4348_4e31,
            lo: 0x8b9d_2f2f_a172_c000,
        };
        let mut name = [0u8; 4];
        name.copy_from_slice(&channel_id.to_be_bytes());
        Self(Uuid128::v5_with_namespace(NS, &name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe task identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TaskUuid(pub Uuid128);

impl TaskUuid {
    pub fn from_task_id(task_id: u64) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x5441_534b_2d49_4431,
            lo: 0xbbe3_1127_1cc5_a900,
        };
        let mut name = [0u8; 8];
        name.copy_from_slice(&task_id.to_be_bytes());
        Self(Uuid128::v5_with_namespace(NS, &name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe device identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DeviceUuid(pub Uuid128);

impl DeviceUuid {
    pub fn from_device_id(device_id: u64) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x4445_5649_4345_2d31,
            lo: 0x8f20_56d1_5a71_b100,
        };
        let mut name = [0u8; 8];
        name.copy_from_slice(&device_id.to_be_bytes());
        Self(Uuid128::v5_with_namespace(NS, &name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe driver package identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DriverPackageUuid(pub Uuid128);

impl DriverPackageUuid {
    pub fn from_package_name(name: &[u8]) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x4452_5650_4b47_2d31,
            lo: 0x8d41_77c2_4fe8_a400,
        };
        Self(Uuid128::v5_with_namespace(NS, name))
    }

    pub fn to_bytes(self) -> [u8; 16] {
        self.0.to_bytes()
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe file identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct FileUuid(pub Uuid128);

impl FileUuid {
    pub fn from_path(path: &[u8]) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x4649_4c45_2d49_4431,
            lo: 0x9084_e9b8_47aa_1200,
        };
        Self(Uuid128::v5_with_namespace(NS, path))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe user/principal identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct UserUuid(pub Uuid128);

impl UserUuid {
    pub fn from_principal_name(name: &[u8]) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x5553_4552_2d49_4431,
            lo: 0xa712_08f3_d5c4_8c00,
        };
        Self(Uuid128::v5_with_namespace(NS, name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

/// Type-safe conversation/session identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct SessionUuid(pub Uuid128);

impl SessionUuid {
    pub fn from_session_id(session_id: u32) -> Self {
        const NS: Uuid128 = Uuid128 {
            hi: 0x5345_5353_2d49_4431,
            lo: 0xb903_21fd_d863_6700,
        };
        let mut name = [0u8; 4];
        name.copy_from_slice(&session_id.to_be_bytes());
        Self(Uuid128::v5_with_namespace(NS, &name))
    }

    pub const fn into_inner(self) -> Uuid128 {
        self.0
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(data: &[u8], seed: u64) -> u64 {
    let mut hash = FNV_OFFSET ^ seed;
    let mut i = 0usize;
    while i < data.len() {
        hash ^= data[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

fn rdrand_u64() -> Option<u64> {
    #[cfg(target_arch = "x86_64")]
    {
        let mut out = 0u64;
        let mut ok: u8;
        let mut attempts = 0u8;
        while attempts < 10 {
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {ok}",
                    val = out(reg) out,
                    ok = lateout(reg_byte) ok,
                    options(nomem, nostack)
                );
            }
            if ok != 0 {
                return Some(out);
            }
            attempts += 1;
        }
        None
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

fn has_rdrand() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let features = core::arch::x86_64::__cpuid(1);
        (features.ecx & (1 << 30)) != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{UUID_NS_GRAPH, Uuid128, Uuid128Gen};

    #[test]
    fn uuid_bytes_roundtrip_preserves_value() {
        let uuid = Uuid128::from_u64_pair(0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210);
        let decoded = Uuid128::from_bytes(uuid.to_bytes());
        assert_eq!(decoded, uuid);
    }

    #[test]
    fn uuid_v5_is_deterministic_for_same_name() {
        let a = Uuid128::v5_service_name(b"graphd");
        let b = Uuid128::v5_service_name(b"graphd");
        assert_eq!(a, b);
    }

    #[test]
    fn uuid_v5_changes_when_name_changes() {
        let a = Uuid128::v5_service_name(b"graphd");
        let b = Uuid128::v5_service_name(b"modeld");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_generator_v5_namespace_wrapper_matches_core_impl() {
        let direct = Uuid128::v5_with_namespace(UUID_NS_GRAPH, b"service:compositor");
        let wrapped = Uuid128Gen::v5_name(UUID_NS_GRAPH, b"service:compositor");
        assert_eq!(direct, wrapped);
    }
}
