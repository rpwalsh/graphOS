#![cfg_attr(not(test), no_std)]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

pub mod bootinfo;
#[path = "ui/direct_present_policy.rs"]
pub mod direct_present_policy;
pub mod display_blit;
pub mod net;
pub mod uuid;

#[cfg(test)]
mod tests {
    use super::bootinfo::{BOOTINFO_VERSION, BootInfo, BootInfoDiag, FramebufferFormat};
    use super::net;

    fn sample_bootinfo() -> BootInfo {
        BootInfo {
            bootinfo_version: BOOTINFO_VERSION,
            bootinfo_size: core::mem::size_of::<BootInfo>() as u32,
            framebuffer_addr: 0xE000_0000,
            framebuffer_width: 1024,
            framebuffer_height: 768,
            framebuffer_stride: 1024,
            framebuffer_format: FramebufferFormat::Bgr,
            memory_regions_ptr: 0x9000,
            memory_regions_count: 4,
            rsdp_addr: 0xF0000,
            kernel_phys_start: 0x10_0000,
            kernel_phys_end: 0x12_0000,
            boot_modules_ptr: 0xA000,
            boot_modules_count: 2,
            package_store_ptr: 0xB000,
            package_store_size: 0x4000,
        }
    }

    #[test]
    fn bootinfo_validation_accepts_well_formed_envelope() {
        let bootinfo = sample_bootinfo();
        assert!(bootinfo.validate_extended().is_empty());
    }

    #[test]
    fn bootinfo_validation_reports_shape_issues() {
        let mut bootinfo = sample_bootinfo();
        bootinfo.framebuffer_width = 0;
        bootinfo.memory_regions_ptr = 0;
        bootinfo.memory_regions_count = 1;
        bootinfo.boot_modules_ptr = 0;
        bootinfo.package_store_size = 0;

        let diag = bootinfo.validate_extended();
        assert!(diag.contains(BootInfoDiag::FB_ZERO_DIMENSION));
        assert!(diag.contains(BootInfoDiag::NO_MEMORY_MAP));
        assert!(diag.contains(BootInfoDiag::BOOT_MODULES_INVALID));
        assert!(diag.contains(BootInfoDiag::PACKAGE_STORE_INVALID));
    }

    #[test]
    fn loopback_socket_round_trip_delivers_payload() {
        net::reset_for_tests();

        let server = net::socket_open(1).expect("server socket");
        let client = net::socket_open(2).expect("client socket");

        assert!(net::socket_bind(1, server, 8080));
        assert!(net::socket_connect(2, client, net::LOOPBACK_IPV4, 8080));
        assert_eq!(net::socket_send(2, client, b"ping"), Some(4));

        let mut buf = [0u8; 16];
        assert_eq!(net::socket_recv(1, server, &mut buf), Some(4));
        assert_eq!(&buf[..4], b"ping");

        assert!(net::socket_close(1, server));
        assert!(net::socket_close(2, client));
    }

    #[test]
    fn arp_table_insert_lookup_and_age_out() {
        let mut table = net::arp::ArpTable::new();
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        table.insert(0xc0a8_0001, mac, 100);
        assert_eq!(table.lookup(0xc0a8_0001), Some(mac));
        table.age_out(101);
        assert_eq!(table.lookup(0xc0a8_0001), None);
    }

    #[test]
    fn ipv4_header_round_trip_is_checksum_clean() {
        let mut header = [0u8; net::ipv4::IPV4_HEADER_BYTES];
        let encoded = net::ipv4::encode_header(
            &mut header,
            84,
            64,
            net::ipv4::IP_PROTOCOL_ICMP,
            net::LOOPBACK_IPV4,
            0x7f00_0002,
            0x1234,
        )
        .expect("header");
        assert_eq!(encoded, net::ipv4::IPV4_HEADER_BYTES);
        let parsed = net::ipv4::parse_header(&header).expect("parsed");
        assert_eq!(parsed.total_len, 84);
        assert_eq!(parsed.protocol, net::ipv4::IP_PROTOCOL_ICMP);
        assert_eq!(parsed.src, net::LOOPBACK_IPV4);
        assert_eq!(parsed.dst, 0x7f00_0002);
        assert_eq!(parsed.ident, 0x1234);
    }

    #[test]
    fn icmp_echo_request_generates_reply() {
        let mut request = [0u8; 64];
        let request_len = net::icmp::build_echo(
            net::icmp::ICMP_ECHO_REQUEST,
            0x4455,
            7,
            b"graphos",
            &mut request,
        )
        .expect("request");

        let mut reply = [0u8; 64];
        let reply_len =
            net::loopback_ping_reply(&request[..request_len], &mut reply).expect("reply");
        let (header, payload) = net::icmp::parse_echo(&reply[..reply_len]).expect("parse reply");
        assert_eq!(header.typ, net::icmp::ICMP_ECHO_REPLY);
        assert_eq!(header.ident, 0x4455);
        assert_eq!(header.sequence, 7);
        assert_eq!(payload, b"graphos");
    }
}
