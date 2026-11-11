//! First-byte dispatch for a mixed listener: SOCKS5 starts with the version
//! byte `0x05`, HTTP starts with an ASCII method letter. The peeked byte is
//! never consumed, so each protocol handler reads the stream from the start.

use super::super::connection::{ControlPlaneHandle, InboundDial};
use super::{http, socks};
use crate::relay::{self, RelayProgress};
use crate::stats::{OutboundKind, StatsManager};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::net::TcpStream;
use tracing::debug;

/// Time allowed for the client to send its first byte before the connection
/// is dropped. Browsers and SOCKS clients send immediately; a stalled socket
/// must not hold a flow permit forever.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn serve_mixed_connection(
    stream: TcpStream,
    client_addr: SocketAddr,
    forced_proxy: Option<&str>,
    handle: &ControlPlaneHandle,
    stats: &Arc<StatsManager>,
) -> anyhow::Result<()> {
    let mut first = [0u8; 1];
    let read = tokio::time::timeout(FIRST_BYTE_TIMEOUT, stream.peek(&mut first)).await;
    let n = match read {
        Ok(Ok(n)) => n,
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            debug!(%client_addr, "inbound client sent no first byte in time");
            return Ok(());
        }
    };
    if n == 0 {
        return Ok(());
    }
    match first[0] {
        0x05 => socks::serve_socks5(stream, client_addr, forced_proxy, handle, stats).await,
        b'A'..=b'Z' => http::serve_http(stream, client_addr, forced_proxy, handle, stats).await,
        other => {
            debug!(%client_addr, first_byte = other, "inbound client spoke neither SOCKS5 nor HTTP");
            Ok(())
        }
    }
}

/// Relay a dialed upstream for one inbound connection, accounting bytes
/// against the resolved outbound like a transparent flow.
pub(super) async fn relay_dialed(
    client: &mut TcpStream,
    dial: InboundDial,
    client_addr: SocketAddr,
    handle: &ControlPlaneHandle,
    stats: &Arc<StatsManager>,
) -> anyhow::Result<()> {
    let kind = OutboundKind::routed(&handle.group_manager.read(), &dial.outbound);
    let _outbound_guard = stats.track_connection(&dial.outbound, kind);
    let (outbound_upload, outbound_download) = stats.byte_counters(&dial.outbound, kind);
    let progress = RelayProgress {
        upload: Arc::new(AtomicU64::new(0)),
        download: Arc::new(AtomicU64::new(0)),
        outbound_upload: Some(outbound_upload),
        outbound_download: Some(outbound_download),
        first_response: None,
        on_transfer: None,
    };
    relay::relay_proxy(client, dial.stream, client_addr, dial.target, progress).await?;
    Ok(())
}
