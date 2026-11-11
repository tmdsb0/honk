pub(in crate::control) fn score_runtime_outcome(
    generation: &honk_outbound::runtime::OutboundRuntimeRegistry,
    error: &anyhow::Error,
) -> crate::group::ScoreOutcome {
    if generation.is_shutdown() {
        crate::group::ScoreOutcome::Shutdown
    } else {
        crate::group::ScoreOutcome::from_error(error)
    }
}
fn overall_dial_timeout(connect_timeout: std::time::Duration) -> std::time::Duration {
    connect_timeout
        .saturating_mul(4)
        .max(std::time::Duration::from_secs(10))
}

pub(in crate::control) fn report_dial_failure_if_current(
    generation: &honk_outbound::runtime::OutboundRuntimeRegistry,
    alive_set: &honk_outbound::alive::AliveDialerSet,
    node_id: uuid::Uuid,
    domain: honk_outbound::alive::ProbeDomain,
    ipver: honk_outbound::alive::IpVersion,
) {
    if generation.is_shutdown() {
        return;
    }
    alive_set.report_unavailable_traffic(node_id, domain, ipver);
    alive_set.record_dial_failure(node_id, domain, ipver);
    alive_set.notify_check_tcp(node_id);
}

pub(in crate::control) mod handoff;
#[cfg(test)]
pub(super) use handoff::TcpFlowKey;
pub(super) use handoff::TcpFlowPins;
pub(crate) use handoff::build_tuples_key;

mod context;

#[cfg_attr(not(feature = "native-api"), path = "observation/inert.rs")]
pub(in crate::control) mod observation;

pub(super) use context::{ConnectionGuard, ControlPlaneHandle};

mod tcp;

mod udp;

mod routing;

#[cfg(test)]
pub(in crate::control) use routing::{RealityOutcome, domain_reality_outcome};

/// Target requested by an inbound listener connection, already parsed by the
/// SOCKS5/HTTP handler: an IP literal, or a domain name the dial must
/// resolve. No sniffing is involved: the client stated the target.
#[derive(Debug, Clone)]
pub(in crate::control) enum InboundTarget {
    Ip(std::net::IpAddr),
    Domain(String),
}

/// A dialed upstream for an inbound connection, ready to relay.
pub(in crate::control) struct InboundDial {
    pub(in crate::control) stream: crate::proxy::ProxyStream,
    pub(in crate::control) target: std::net::SocketAddr,
    /// Outbound name for stats/accounting: the forced proxy, or the routed
    /// outbound when the listener falls back to global routing.
    pub(in crate::control) outbound: String,
}

/// Dial failure modes a protocol handler must distinguish: a policy refusal
/// gets a protocol refusal reply, anything else a generic dial-failure reply.
#[derive(Debug, thiserror::Error)]
pub(in crate::control) enum InboundDialError {
    #[error("connection blocked by outbound '{0}'")]
    Blocked(String),
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}
