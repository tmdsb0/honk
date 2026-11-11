//! Mixed (HTTP + SOCKS5) inbound listener configuration.
//!
//! One listener binds one local port and pins its outbound to a single
//! proxy target: a node name, a group name, `direct`, or `block`. An absent
//! or empty `proxy` falls back to the global routing rules. Listener names
//! and proxy names are kept verbatim: no normalization, no ASCII folding,
//! so Chinese and emoji names survive unchanged.

use serde::{Deserialize, Serialize};

/// Default bind address for a listener: loopback only, never `0.0.0.0`.
/// Operators must opt in to LAN exposure with an explicit `listen`.
pub fn default_listen() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// One inbound listener: a named `mixed` (HTTP + SOCKS5) TCP port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerConfig {
    /// Listener name, unique across `listeners` (exact string match).
    pub name: String,
    /// Listener protocol. MVP: only `mixed` is accepted.
    #[serde(rename = "type", default)]
    pub listener_type: ListenerType,
    /// Local address to bind. Defaults to `127.0.0.1`; `0.0.0.0` is
    /// allowed when set explicitly.
    #[serde(default = "default_listen")]
    pub listen: std::net::IpAddr,
    /// Local TCP port to bind, 1..=65535. Required in dae syntax.
    pub port: u16,
    /// Outbound pin: a node name, a group name, `direct`, or `block`.
    /// Empty/absent falls back to the global routing rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// Listener protocol. MVP: only [`ListenerType::Mixed`] exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ListenerType {
    #[default]
    Mixed,
}
