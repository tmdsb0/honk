Added

- Mixed (HTTP + SOCKS5) inbound listeners: `listeners { ... }` in `config.dae` binds one TCP port per listener, each pinned to a node, group, `direct`, or `block` via `proxy:`, or falling back to global routing when unset.
