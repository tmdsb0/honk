//! SOCKS5 server (RFC 1928), no-auth only, TCP CONNECT only.
//!
//! UDP ASSOCIATE is refused with a proper SOCKS error reply
//! (`REP=0x07`, command not supported) and the connection is closed; the
//! MVP has no UDP association table.

use super::super::connection::{ControlPlaneHandle, InboundDialError, InboundTarget};
use super::mixed;
use crate::stats::StatsManager;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

/// SOCKS5 reply codes (RFC 1928 §6).
const REP_SUCCESS: u8 = 0x00;
const REP_NOT_ALLOWED: u8 = 0x02;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_CONNECTION_REFUSED: u8 = 0x05;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;

pub(super) async fn serve_socks5(
    mut stream: TcpStream,
    client_addr: SocketAddr,
    forced_proxy: Option<&str>,
    handle: &ControlPlaneHandle,
    stats: &Arc<StatsManager>,
) -> anyhow::Result<()> {
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    let [version, nmethods] = greeting;
    if version != 0x05 {
        anyhow::bail!("SOCKS5 greeting version {version:#x}");
    }
    let mut methods = vec![0u8; nmethods as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0x00) {
        // No acceptable method: no-auth is the only method this server offers.
        stream.write_all(&[0x05, 0xFF]).await?;
        return Ok(());
    }
    stream.write_all(&[0x05, 0x00]).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    let [version, cmd, _reserved, atyp] = head;
    if version != 0x05 {
        anyhow::bail!("SOCKS5 request version {version:#x}");
    }
    let (target, target_port) = match read_address(&mut stream, atyp).await {
        Ok(address) => address,
        Err(error) => {
            debug!(%error, %client_addr, "SOCKS5 request address unreadable");
            send_reply(&mut stream, REP_ADDRESS_TYPE_NOT_SUPPORTED, None).await?;
            return Ok(());
        }
    };
    match cmd {
        0x01 => {}
        0x03 => {
            // UDP ASSOCIATE: refuse cleanly, never hang or half-serve.
            send_reply(&mut stream, REP_COMMAND_NOT_SUPPORTED, None).await?;
            return Ok(());
        }
        other => {
            debug!(%client_addr, cmd = other, "SOCKS5 command not supported");
            send_reply(&mut stream, REP_COMMAND_NOT_SUPPORTED, None).await?;
            return Ok(());
        }
    }

    let dial = match handle
        .dial_for_inbound(target, target_port, forced_proxy, client_addr.ip())
        .await
    {
        Ok(dial) => dial,
        Err(InboundDialError::Blocked(_)) => {
            send_reply(&mut stream, REP_NOT_ALLOWED, None).await?;
            return Ok(());
        }
        Err(InboundDialError::Failed(error)) => {
            debug!(%error, %client_addr, "inbound SOCKS5 dial failed");
            let rep = if error.downcast_ref::<std::io::Error>().is_some_and(|io| {
                matches!(
                    io.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::AddrNotAvailable
                )
            }) {
                REP_HOST_UNREACHABLE
            } else {
                REP_CONNECTION_REFUSED
            };
            send_reply(&mut stream, rep, None).await?;
            return Ok(());
        }
    };
    let bound = stream.local_addr().ok();
    send_reply(&mut stream, REP_SUCCESS, bound).await?;
    mixed::relay_dialed(&mut stream, dial, client_addr, handle, stats).await
}

async fn read_address(stream: &mut TcpStream, atyp: u8) -> anyhow::Result<(InboundTarget, u16)> {
    let target = match atyp {
        0x01 => {
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await?;
            InboundTarget::Ip(IpAddr::V4(Ipv4Addr::from(addr)))
        }
        0x03 => {
            let len = stream.read_u8().await? as usize;
            let mut domain = vec![0u8; len];
            stream.read_exact(&mut domain).await?;
            InboundTarget::Domain(String::from_utf8(domain)?)
        }
        0x04 => {
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await?;
            InboundTarget::Ip(IpAddr::V6(Ipv6Addr::from(addr)))
        }
        other => anyhow::bail!("SOCKS5 address type {other:#x} not supported"),
    };
    let port = stream.read_u16().await?;
    Ok((target, port))
}

/// `VER REP RSV ATYP BND.ADDR BND.PORT`; the bound address is the listener
/// address the client connected to, or `0.0.0.0:0` when it is unavailable.
async fn send_reply(
    stream: &mut TcpStream,
    rep: u8,
    bound: Option<SocketAddr>,
) -> anyhow::Result<()> {
    let mut reply = vec![0x05, rep, 0x00];
    match bound {
        Some(SocketAddr::V4(addr)) => {
            reply.push(0x01);
            reply.extend_from_slice(&addr.ip().octets());
            reply.extend_from_slice(&addr.port().to_be_bytes());
        }
        Some(SocketAddr::V6(addr)) => {
            reply.push(0x04);
            reply.extend_from_slice(&addr.ip().octets());
            reply.extend_from_slice(&addr.port().to_be_bytes());
        }
        None => {
            reply.push(0x01);
            reply.extend_from_slice(&[0, 0, 0, 0]);
            reply.extend_from_slice(&[0, 0]);
        }
    }
    stream.write_all(&reply).await?;
    stream.flush().await?;
    Ok(())
}
