//! HTTP proxy server: CONNECT tunnelling and plain HTTP forwarding via the
//! Host header (or absolute-form request target). No auth, TCP only.

use super::super::connection::{ControlPlaneHandle, InboundDialError, InboundTarget};
use super::mixed;
use crate::stats::StatsManager;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

/// Cap on the request head (request line + headers). Browsers send a few
/// hundred bytes; anything larger is not a proxy request worth holding a
/// flow permit for.
const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn serve_http(
    mut stream: TcpStream,
    client_addr: SocketAddr,
    forced_proxy: Option<&str>,
    handle: &ControlPlaneHandle,
    stats: &Arc<StatsManager>,
) -> anyhow::Result<()> {
    let (head, body_start) = match read_request_head(&mut stream).await {
        Ok(found) => found,
        Err(error) => {
            debug!(%error, %client_addr, "HTTP request head unreadable");
            return Ok(());
        }
    };
    let head_text = match std::str::from_utf8(&head) {
        Ok(text) => text,
        Err(_) => {
            write_response(&mut stream, "400 Bad Request").await?;
            return Ok(());
        }
    };
    let mut lines = head_text.lines();
    let Some(request_line) = lines.next() else {
        write_response(&mut stream, "400 Bad Request").await?;
        return Ok(());
    };
    let mut parts = request_line.split_ascii_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        write_response(&mut stream, "400 Bad Request").await?;
        return Ok(());
    };
    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':').map(|(k, v)| (k.trim(), v.trim())))
        .collect();
    let host_header = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| *value);

    // The tuple's port slot is dead: the port always comes from
    // `split_host_port` below, so only host and CONNECT-ness are kept here.
    let (host, is_connect) = if method.eq_ignore_ascii_case("CONNECT") {
        (target.to_owned(), true)
    } else if let Some(rest) = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        // Absolute-form target: authority is up to the first `/`.
        let authority = rest.split('/').next().unwrap_or(rest);
        (authority.to_owned(), false)
    } else {
        match host_header {
            Some(host) => (host.to_owned(), false),
            None => {
                write_response(&mut stream, "400 Bad Request").await?;
                return Ok(());
            }
        }
    };
    // `host` may still carry `:port`; CONNECT defaults to 443, plain HTTP to 80.
    let (host, port) = split_host_port(&host);
    let port = port.unwrap_or(if is_connect { 443 } else { 80 });
    let target = match host.parse::<IpAddr>() {
        Ok(ip) => InboundTarget::Ip(ip),
        Err(_) => InboundTarget::Domain(host),
    };

    let mut dial = match handle
        .dial_for_inbound(target, port, forced_proxy, client_addr.ip())
        .await
    {
        Ok(dial) => dial,
        Err(InboundDialError::Blocked(_)) => {
            write_response(&mut stream, "403 Forbidden").await?;
            return Ok(());
        }
        Err(InboundDialError::Failed(error)) => {
            debug!(%error, %client_addr, "inbound HTTP dial failed");
            write_response(&mut stream, "502 Bad Gateway").await?;
            return Ok(());
        }
    };
    if is_connect {
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    } else {
        // Forward the request head exactly as received; any bytes already
        // read past the header terminator belong to the body and go first.
        dial.stream.stream.write_all(&head[..body_start]).await?;
        if body_start < head.len() {
            dial.stream.stream.write_all(&head[body_start..]).await?;
        }
        dial.stream.stream.flush().await?;
    }
    mixed::relay_dialed(&mut stream, dial, client_addr, handle, stats).await
}

/// Read until the end of the request head (`\r\n\r\n`). Returns the bytes
/// read and the offset where the body starts.
async fn read_request_head(stream: &mut TcpStream) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if buf.len() > MAX_REQUEST_HEAD_BYTES {
            anyhow::bail!("HTTP request head exceeds {MAX_REQUEST_HEAD_BYTES} bytes");
        }
        if let Some(end) = find_header_end(&buf) {
            return Ok((buf, end));
        }
        let n = tokio::time::timeout(HEADER_READ_TIMEOUT, stream.read(&mut chunk)).await??;
        if n == 0 {
            anyhow::bail!("HTTP client closed before sending a complete request head");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

async fn write_response(stream: &mut TcpStream, status: &str) -> anyhow::Result<()> {
    stream
        .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
        .await?;
    stream.flush().await?;
    Ok(())
}

/// Split `host[:port]` (or `[v6]:port`) into host and optional port. A bare
/// IPv6 literal without brackets is returned as host with no port.
fn split_host_port(host: &str) -> (String, Option<u16>) {
    if let Some(bracketed) = host.strip_prefix('[') {
        if let Some((inside, rest)) = bracketed.split_once(']') {
            let port = rest.strip_prefix(':').and_then(|p| p.parse().ok());
            return (inside.to_owned(), port);
        }
        return (host.to_owned(), None);
    }
    if host.matches(':').count() == 1
        && let Some((h, p)) = host.rsplit_once(':')
        && let Ok(port) = p.parse()
    {
        return (h.to_owned(), Some(port));
    }
    (host.to_owned(), None)
}
