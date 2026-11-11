//! Mixed (HTTP+SOCKS5) inbound listeners (`listeners { ... }`).
//!
//! These sockets are ordinary host-netns listeners, modelled on
//! `dns.bind`: they never enter `daens` and carry no proxy mark. Each
//! listener pins its connections to one outbound (`proxy:`) or falls back
//! to global routing. TCP only, no auth, no UDP.

mod http;
mod mixed;
mod socks;
#[cfg(test)]
mod tests;

use super::ConnectionGuard;
use super::connection::ControlPlaneHandle;
use crate::stats::StatsManager;
use honk_config::{ListenerConfig, ListenerType};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tracing::{debug, info};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerPhase {
    Running,
    StopAccepting,
    Abort,
}

/// Per-listener share of the process TCP flow budget. Mirrors the standalone
/// DNS listener: listener flows count against the shared
/// `concurrency_limit`, with a quarter reserved for this listener class so
/// one busy listener cannot starve the transparent datapath.
const fn standalone_tcp_capacity(total_connection_capacity: usize) -> usize {
    if total_connection_capacity == 0 {
        0
    } else {
        let quarter = total_connection_capacity / 4;
        if quarter == 0 { 1 } else { quarter }
    }
}

/// Synchronously bound socket. Keeping binding separate from spawning makes
/// startup atomic: no supervisor exists until every configured listener has
/// successfully bound.
pub(super) struct BoundInboundListener {
    config: ListenerConfig,
    listener: std::net::TcpListener,
}

impl BoundInboundListener {
    pub(super) fn bind(config: &ListenerConfig) -> io::Result<Self> {
        debug_assert!(matches!(config.listener_type, ListenerType::Mixed));
        let addr = SocketAddr::new(config.listen, config.port);
        let listener = bind_tcp_listener(addr)?;
        Ok(Self {
            config: config.clone(),
            listener,
        })
    }

    pub(super) fn spawn(
        self,
        handle: ControlPlaneHandle,
        connection_limit: Arc<Semaphore>,
        stats: Arc<StatsManager>,
        drain: Arc<super::drain::DrainTracker>,
    ) -> io::Result<InboundListener> {
        let Self { config, listener } = self;
        let local_addr = listener.local_addr()?;
        let listener = TcpListener::from_std(listener)?;
        let (phase, phase_rx) = watch::channel(ListenerPhase::Running);
        let standalone_limit = Arc::new(Semaphore::new(standalone_tcp_capacity(
            connection_limit.available_permits(),
        )));
        let mut supervisors = JoinSet::new();
        supervisors.spawn(run_accept_supervisor(
            config.clone(),
            listener,
            handle,
            connection_limit,
            standalone_limit,
            stats,
            drain,
            phase_rx,
        ));
        info!(
            listener = %config.name,
            %local_addr,
            proxy = config
                .proxy
                .as_deref()
                .filter(|proxy| !proxy.is_empty())
                .unwrap_or("<global routing>"),
            "Inbound mixed listener started"
        );
        Ok(InboundListener { phase, supervisors })
    }

    #[cfg(test)]
    pub(super) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// Process-scoped supervisor ownership retained by `ControlPlane::run`.
pub(super) struct InboundListener {
    phase: watch::Sender<ListenerPhase>,
    supervisors: JoinSet<anyhow::Result<()>>,
}

impl InboundListener {
    /// Close accept admission while allowing already tracked children to
    /// finish as part of the control plane's ordinary bounded drain.
    pub(super) fn stop_accepting(&self) {
        let _ = self.phase.send(ListenerPhase::StopAccepting);
    }

    /// Force-cancel any child that outlived the bounded drain and join every
    /// supervisor.
    pub(super) async fn abort_and_join(&mut self) -> anyhow::Result<()> {
        let _ = self.phase.send(ListenerPhase::Abort);
        let mut failure = None;
        while let Some(result) = self.supervisors.join_next().await {
            if let Err(error) = result
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
            {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for InboundListener {
    fn drop(&mut self) {
        let _ = self.phase.send(ListenerPhase::Abort);
        self.supervisors.abort_all();
    }
}

/// All configured listeners, spawned together in `prepare_epoch` (before
/// datapath admission, like the standalone DNS listener) and torn down
/// together in `stop_network_epoch`.
pub(super) struct InboundListenerSet {
    listeners: Vec<InboundListener>,
}

impl InboundListenerSet {
    pub(super) fn spawn_all(
        bound: Vec<BoundInboundListener>,
        handle: ControlPlaneHandle,
        connection_limit: Arc<Semaphore>,
        stats: Arc<StatsManager>,
        drain: Arc<super::drain::DrainTracker>,
    ) -> io::Result<Self> {
        let mut listeners = Vec::with_capacity(bound.len());
        for item in bound {
            listeners.push(item.spawn(
                handle.clone(),
                Arc::clone(&connection_limit),
                Arc::clone(&stats),
                Arc::clone(&drain),
            )?);
        }
        Ok(Self { listeners })
    }

    pub(super) fn stop_accepting(&self) {
        for listener in &self.listeners {
            listener.stop_accepting();
        }
    }

    pub(super) async fn abort_and_join(&mut self) -> anyhow::Result<()> {
        let mut failure = None;
        for listener in &mut self.listeners {
            if let Err(error) = listener.abort_and_join().await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

fn bind_tcp_listener(addr: SocketAddr) -> io::Result<std::net::TcpListener> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    if addr.is_ipv6() && addr.ip().is_unspecified() {
        socket.set_only_v6(false)?;
    }
    // SO_REUSEADDR permits an immediate process restart across accepted
    // connections in TIME_WAIT; without SO_REUSEPORT it cannot share a live
    // listener tuple.
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

#[allow(clippy::too_many_arguments)]
async fn run_accept_supervisor(
    config: ListenerConfig,
    listener: TcpListener,
    handle: ControlPlaneHandle,
    connection_limit: Arc<Semaphore>,
    standalone_limit: Arc<Semaphore>,
    stats: Arc<StatsManager>,
    drain: Arc<super::drain::DrainTracker>,
    mut phase: watch::Receiver<ListenerPhase>,
) -> anyhow::Result<()> {
    let mut children = JoinSet::new();
    let mut failure = None;
    // A persistent accept error repeats every iteration; warn once per episode.
    let mut accept_failing = false;

    loop {
        tokio::select! {
            biased;
            changed = phase.changed() => {
                if changed.is_err() || *phase.borrow() != ListenerPhase::Running {
                    break;
                }
            }
            completed = children.join_next(), if !children.is_empty() => {
                retain_child_result(&mut failure, completed, false);
            }
            accepted = listener.accept() => {
                let (stream, client_addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        crate::logging::warn_on_entry!(
                            !std::mem::replace(&mut accept_failing, true),
                            error_kind = ?error.kind(),
                            listener = %config.name,
                            "inbound listener accept failed"
                        );
                        continue;
                    }
                };
                if std::mem::replace(&mut accept_failing, false) {
                    info!(listener = %config.name, "inbound listener accept recovered");
                }
                if *phase.borrow() != ListenerPhase::Running || drain.should_reject() {
                    continue;
                }
                let standalone_permit = match Arc::clone(&standalone_limit).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        debug!(listener = %config.name, %client_addr, "dropping inbound connection at listener capacity");
                        continue;
                    }
                };
                let permit = match Arc::clone(&connection_limit).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        stats.record_tcp_capacity_rejection();
                        debug!(listener = %config.name, %client_addr, "dropping inbound connection at capacity");
                        continue;
                    }
                };
                let guard = ConnectionGuard::new(Arc::clone(&drain));
                let child_handle = handle.clone();
                let child_stats = Arc::clone(&stats);
                // An empty `proxy:` string means "no pin", same as absent
                // (config validation treats it that way too).
                let forced_proxy = config.proxy.clone().filter(|proxy| !proxy.is_empty());
                children.spawn(async move {
                    let _permit = permit;
                    let _standalone_permit = standalone_permit;
                    let _guard = guard;
                    if let Err(error) = mixed::serve_mixed_connection(
                        stream,
                        client_addr,
                        forced_proxy.as_deref(),
                        &child_handle,
                        &child_stats,
                    )
                    .await
                    {
                        debug!(%error, %client_addr, "inbound connection ended");
                    }
                });
            }
        }
    }

    drop(listener);
    finish_children(&mut children, &mut phase, &mut failure).await;
    failure.map_or(Ok(()), Err)
}

async fn finish_children(
    children: &mut JoinSet<()>,
    phase: &mut watch::Receiver<ListenerPhase>,
    failure: &mut Option<anyhow::Error>,
) {
    let mut aborted = *phase.borrow() == ListenerPhase::Abort;
    if aborted {
        children.abort_all();
    }

    while !children.is_empty() {
        tokio::select! {
            changed = phase.changed(), if !aborted => {
                if changed.is_err() || *phase.borrow() == ListenerPhase::Abort {
                    aborted = true;
                    children.abort_all();
                }
            }
            completed = children.join_next() => retain_child_result(failure, completed, aborted),
        }
    }
}

fn retain_child_result(
    failure: &mut Option<anyhow::Error>,
    completed: Option<Result<(), tokio::task::JoinError>>,
    aborted: bool,
) {
    if let Some(Err(error)) = completed
        && (!aborted || !error.is_cancelled())
    {
        debug!("inbound child join failed");
        failure.get_or_insert_with(|| anyhow::Error::new(error).context("inbound connection"));
    }
}
