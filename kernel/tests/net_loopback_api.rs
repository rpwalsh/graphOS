// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use graphos_kernel::net;

#[test]
fn socket_api_loopback_roundtrip_across_crate_boundary() {
    let server = net::socket_open(1001).expect("server socket should open");
    let client = net::socket_open(1002).expect("client socket should open");

    assert!(net::socket_bind(1001, server, 38080));
    assert!(net::socket_connect(1002, client, net::LOOPBACK_IPV4, 38080));
    assert_eq!(net::socket_send(1002, client, b"hello"), Some(5));

    let mut buf = [0u8; 16];
    assert_eq!(net::socket_recv(1001, server, &mut buf), Some(5));
    assert_eq!(&buf[..5], b"hello");

    assert!(net::socket_close(1001, server));
    assert!(net::socket_close(1002, client));
}
