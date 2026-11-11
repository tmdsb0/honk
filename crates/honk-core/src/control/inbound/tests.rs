//! Mock-eBPF loopback tests for the mixed inbound listener: a browser-side
//! client speaks SOCKS5/HTTP to the listener, the listener dials a local
//! target (direct) or a local SOCKS5 node, and bytes round-trip.
//!
//! The tests build a real `ControlPlane` (mock eBPF backend) and a real
//! `ControlPlaneHandle`, so `dial_for_inbound` runs its production path;
//! only the far-end servers are test doubles.

use super::*;
use crate::control::tests::support::control_plane;
use crate::stats::StatsManager;
use honk_config::node::{Node, OutboundConfig};
use honk_config::types::NodeProtocol;
use honk_config::{Config, ListenerConfig, ListenerType};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

async fn run_echo_server(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let (mut reader, mut writer) = stream.into_split();
            let _ = tokio::io::copy(&mut reader, &mut writer).await;
        });
    }
}

/// Minimal upstream SOCKS5 server (no-auth CONNECT only) standing in for a
/// real proxy node.
async fn run_test_socks5_server(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            if let Err(error) = serve_test_socks5(stream).await {
                debug!(%error, "test SOCKS5 session ended");
            }
        });
    }
}

async fn serve_test_socks5(mut stream: TcpStream) -> anyhow::Result<()> {
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    let mut methods = vec![0u8; greeting[1] as usize];
    stream.read_exact(&mut methods).await?;
    stream.write_all(&[0x05, 0x00]).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    anyhow::ensure!(head[1] == 0x01, "test SOCKS5 only implements CONNECT");
    let target = match head[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
            SocketAddr::new(IpAddr::V4(Ipv4Addr::from(addr)), stream.read_u16().await?)
        }
        0x03 => {
            let len = stream.read_u8().await? as usize;
            let mut domain = vec![0u8; len];
            stream.read_exact(&mut domain).await?;
            let port = stream.read_u16().await?;
            let host = String::from_utf8(domain)?;
            tokio::net::lookup_host((host.as_str(), port))
                .await?
                .next()
                .ok_or_else(|| anyhow::anyhow!("test SOCKS5: domain did not resolve"))?
        }
        0x04 => {
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
            SocketAddr::new(
                IpAddr::V6(std::net::Ipv6Addr::from(addr)),
                stream.read_u16().await?,
            )
        }
        other => anyhow::bail!("test SOCKS5: bad address type {other:#x}"),
    };
    let mut upstream = TcpStream::connect(target).await?;
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

async fn spawn_test_listener(
    handle: ControlPlaneHandle,
    proxy: Option<String>,
) -> (SocketAddr, InboundListenerSet) {
    let config = ListenerConfig {
        name: "test-mixed".into(),
        listener_type: ListenerType::Mixed,
        listen: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        proxy,
    };
    let bound = BoundInboundListener::bind(&config).expect("test listener binds");
    let addr = bound.local_addr().expect("test listener address");
    let set = InboundListenerSet::spawn_all(
        vec![bound],
        handle,
        Arc::new(Semaphore::new(64)),
        Arc::new(StatsManager::new()),
        Arc::new(super::super::drain::DrainTracker::new()),
    )
    .expect("test listener spawns");
    (addr, set)
}

/// SOCKS5 CONNECT through the listener; returns the tunnelled stream.
async fn socks5_connect(listener: SocketAddr, target: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(listener).await.unwrap();
    stream.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut method_reply = [0u8; 2];
    stream.read_exact(&mut method_reply).await.unwrap();
    assert_eq!(method_reply, [0x05, 0x00]);
    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    let IpAddr::V4(ip) = target.ip() else {
        panic!("tests use IPv4 targets");
    };
    request.extend_from_slice(&ip.octets());
    request.extend_from_slice(&target.port().to_be_bytes());
    stream.write_all(&request).await.unwrap();
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 0x05);
    assert_eq!(head[1], 0x00, "SOCKS5 CONNECT was refused");
    assert_eq!(head[3], 0x01);
    let mut bound = [0u8; 6];
    stream.read_exact(&mut bound).await.unwrap();
    stream
}

async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    stream.write_all(payload).await.unwrap();
    let mut buf = vec![0u8; payload.len()];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, payload);
}

#[tokio::test]
async fn mixed_listener_socks5_connect_to_direct_echo() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(run_echo_server(echo));
    let plane = control_plane(Config::default());
    let (addr, mut listeners) = spawn_test_listener(plane.spawn_handle(), None).await;
    let mut client = socks5_connect(addr, echo_addr).await;
    assert_echo(&mut client, b"hello-inbound").await;
    listeners.abort_and_join().await.unwrap();
}

#[tokio::test]
async fn mixed_listener_http_connect_to_direct_echo() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(run_echo_server(echo));
    let plane = control_plane(Config::default());
    let (addr, mut listeners) = spawn_test_listener(plane.spawn_handle(), None).await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(format!("CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut head = [0u8; 39];
    client.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 200 Connection Established\r\n\r\n");
    assert_echo(&mut client, b"hello-http").await;
    listeners.abort_and_join().await.unwrap();
}

#[tokio::test]
async fn mixed_listener_plain_http_forwarding_to_direct_echo() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(run_echo_server(echo));
    let plane = control_plane(Config::default());
    let (addr, mut listeners) = spawn_test_listener(plane.spawn_handle(), None).await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    let request = format!("GET /hello HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n");
    client.write_all(request.as_bytes()).await.unwrap();
    // The echo target returns the forwarded request verbatim.
    let mut buf = vec![0u8; request.len()];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, request.as_bytes());
    listeners.abort_and_join().await.unwrap();
}

#[tokio::test]
async fn mixed_listener_socks5_via_local_socks5_node() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(run_echo_server(echo));
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(run_test_socks5_server(upstream));
    let mut node = Node {
        name: "local-socks5".into(),
        host: "127.0.0.1".into(),
        port: upstream_addr.port(),
        address: upstream_addr.to_string(),
        outbound: OutboundConfig::from_protocol(NodeProtocol::Socks5),
        ..Default::default()
    };
    node.id = node.derive_id();
    let mut config = Config::default();
    config.nodes.push(node);
    let plane = control_plane(config);
    let (addr, mut listeners) =
        spawn_test_listener(plane.spawn_handle(), Some("local-socks5".into())).await;
    let mut client = socks5_connect(addr, echo_addr).await;
    assert_echo(&mut client, b"hello-via-node").await;
    listeners.abort_and_join().await.unwrap();
}

#[tokio::test]
async fn mixed_listener_socks5_udp_associate_refused() {
    let plane = control_plane(Config::default());
    let (addr, mut listeners) = spawn_test_listener(plane.spawn_handle(), None).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut method_reply = [0u8; 2];
    stream.read_exact(&mut method_reply).await.unwrap();
    assert_eq!(method_reply, [0x05, 0x00]);
    stream
        .write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 0x05);
    assert_eq!(
        head[1], 0x07,
        "UDP ASSOCIATE must be refused with command-not-supported"
    );
    let mut bound = [0u8; 6];
    stream.read_exact(&mut bound).await.unwrap();
    // The refusal closes the connection; it must not hang.
    let mut one = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut one)).await;
    assert!(
        matches!(closed, Ok(Ok(0))),
        "UDP ASSOCIATE refusal must close promptly"
    );
    listeners.abort_and_join().await.unwrap();
}
