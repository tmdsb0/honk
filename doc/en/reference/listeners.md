# Listeners reference

`listeners { ... }` binds local mixed (HTTP + SOCKS5) inbound ports, one
per browser profile or client. Each listener pins its outbound to a single
proxy target; an absent or empty `proxy` falls back to the global routing
rules. TCP only, no authentication, no UDP in this build.

## Syntax

```dae
listeners {
    mixed0 {
        type: mixed
        listen: 0.0.0.0
        port: 12000
        proxy: '菲律宾-1'
    }
    mixed1 {
        type: mixed
        port: 13000
        proxy: '🇵🇭 菲律宾-2'
    }
    mixed2 {
        type: mixed
        port: 12001
        proxy: '🇻🇳 越南'
    }
}
```

## Keys

| dae key | Internal field | Default | Meaning |
| ------- | -------------- | ------- | ------- |
| (section name) | `name` | required | Listener name, unique across `listeners` (exact string match). |
| `type` | `listener_type` | `mixed` | Listener protocol. Only `mixed` is accepted; any other value is a parse error. |
| `listen` | `listen` | `127.0.0.1` | Local IP address to bind. `0.0.0.0` is allowed when set explicitly. |
| `port` | `port` | required | Local TCP port, 1..=65535. `0` is rejected. |
| `proxy` | `proxy` | `null` | Outbound pin: a node name, a group name, `direct`, or `block`. Empty/absent falls back to global routing. |

Listener names and proxy names are kept verbatim — Chinese and emoji are
preserved exactly, with no normalization or ASCII folding. Two listeners
may share a port only on different listen addresses; the same
`(listen, port)` pair is rejected, as is a duplicate name.

## Choosing the proxy target

> **Warning: do not point a listener at a `score`, `urltest`, or
> `loadbalance` group.** Those policies can switch the selected leaf
> mid-session, changing the egress IP and breaking fingerprint
> consistency (expect sign-in prompts, captchas, or bans). Pin each
> listener to **one node**, or to a group with `policy: fixed(0)` (or a
> Selector the operator never touches).

A `proxy` value must name an existing node, an existing group, `direct`,
or `block`; anything else fails validation. `direct` sends the listener's
traffic out unproxied; `block` drops it.

## Notes

- `listen` defaults to loopback. Set `listen: 0.0.0.0` (or a LAN address)
  explicitly to expose a listener to the LAN; there is no authentication,
  so only do this on a trusted network.
- Listener changes are restart-required: editing `listeners { }` and
  sending SIGHUP does not rebind ports.
- A listener port must not collide with `global.tproxy_port` (default
  12345), `dns.bind`, or the Clash `external_controller` port.
