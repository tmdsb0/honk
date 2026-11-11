//! `listeners { ... }` section: one named `mixed` listener per block.
//!
//! ```dae
//! listeners {
//!     mixed0 {
//!         type: mixed
//!         listen: 0.0.0.0
//!         port: 12000
//!         proxy: '菲律宾-1'
//!     }
//! }
//! ```
//!
//! `type` defaults to `mixed` and is the only accepted value in the MVP;
//! `listen` defaults to `127.0.0.1`; `port` is required. Names and proxy
//! values are kept verbatim, including Chinese and emoji.

use std::collections::HashMap;

use super::cursor::Segment;
use super::diagnostics::ParserDiagnostics;
use super::read::{self, Text};
use crate::diagnostic::Severity;
use crate::listener::{ListenerConfig, ListenerType};

/// Parse every `listeners { ... }` section into listener configs, in
/// declaration order. Duplicate names/addresses and unknown proxy targets
/// are not rejected here; [`crate::Config::validate`] owns those checks so
/// the serde (JSON/YAML/TOML) paths share them.
pub(super) fn parse_listeners_section(
    section: &[Segment<'_, '_>],
    diagnostics: &mut ParserDiagnostics<'_>,
) -> Result<Vec<ListenerConfig>, super::ParseFailure> {
    let mut listeners = Vec::new();

    for root in section {
        let Some(body) = root.body_with(super::cursor::BodySyntax::Declarations) else {
            continue;
        };
        for segment in body {
            diagnostics.at_section("listeners", Text::segment(&segment));
            let Some(name_text) = read::block_header(&segment) else {
                Text::segment(&segment).notice(
                    diagnostics,
                    Severity::Warning,
                    "unknown-statement",
                    "listener declaration requires a block",
                );
                continue;
            };
            let index = listeners.len() + 1;
            let name = name_text.unquote().raw().trim().to_owned();

            let mut fields: HashMap<&str, Text<'_, '_>> = HashMap::new();
            for statement in
                read::child_statements(&segment, diagnostics, super::cursor::BodySyntax::Statements)
            {
                let Some((key, value)) = statement.kv() else {
                    statement.notice(
                        diagnostics,
                        Severity::Warning,
                        "unknown-statement",
                        "unknown listener statement ignored",
                    );
                    continue;
                };
                if !["type", "listen", "port", "proxy"].contains(&key.raw()) {
                    key.notice(
                        diagnostics,
                        Severity::Warning,
                        "unknown-key",
                        "unknown listener key ignored",
                    );
                    continue;
                }
                let key = key.raw().trim();
                diagnostics.register_field(key, value);
                fields.insert(key, value.unquote());
            }

            let mut listener = ListenerConfig {
                name,
                listener_type: ListenerType::Mixed,
                listen: crate::listener::default_listen(),
                port: 0,
                proxy: None,
            };
            if let Some(value) = fields.get("type").copied() {
                let raw = value.raw().trim();
                if !raw.eq_ignore_ascii_case("mixed") {
                    return Err(listener_scalar_error(
                        value,
                        index,
                        "type",
                        "listener type must be 'mixed'; other listener types are not supported",
                    )
                    .into());
                }
                listener.listener_type = ListenerType::Mixed;
            }
            if let Some(value) = fields.get("listen").copied() {
                listener.listen = value
                    .raw()
                    .trim()
                    .parse::<std::net::IpAddr>()
                    .map_err(|_| {
                        listener_scalar_error(
                            value,
                            index,
                            "listen",
                            "listen must be an IP address",
                        )
                    })?;
            }
            let Some(port) = fields.get("port").copied() else {
                return Err(listener_scalar_error(
                    name_text,
                    index,
                    "port",
                    "listener port is required",
                )
                .into());
            };
            listener.port = port.raw().trim().parse::<u16>().map_err(|_| {
                listener_scalar_error(
                    port,
                    index,
                    "port",
                    "listener port must be an integer between 1 and 65535",
                )
            })?;
            if listener.port == 0 {
                return Err(listener_scalar_error(
                    port,
                    index,
                    "port",
                    "listener port must be an integer between 1 and 65535",
                )
                .into());
            }
            if let Some(value) = fields.get("proxy").copied() {
                let proxy = value.raw().to_owned();
                if !proxy.trim().is_empty() {
                    listener.proxy = Some(proxy);
                }
            }

            listeners.push(listener);
        }
    }

    Ok(listeners)
}

fn listener_scalar_error(
    value: Text<'_, '_>,
    index: usize,
    field: &'static str,
    message: &'static str,
) -> crate::error::DetailedConfigError {
    let mut error = crate::error::DetailedConfigError::new(
        crate::error::ErrorCategory::Validation,
        "invalid-config-value",
        value.source.reference(),
        crate::diagnostic::SettingPath::new("listeners")
            .index(index)
            .field(field),
        message,
    );
    error.diagnostic.line = Some(value.source.location(value.span.start).0);
    error
}
