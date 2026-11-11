# 监听器参考

`listeners { ... }` 绑定本地 mixed（HTTP + SOCKS5）入站端口，每个浏览器
配置文件或客户端一个端口。每个监听器将其出站固定到单个代理目标；`proxy`
留空或缺省时回退到全局路由规则。本版本仅支持 TCP，无认证，不支持 UDP。

## 语法

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

## 字段

| dae 键 | 内部字段 | 默认值 | 含义 |
| ------- | -------------- | ------- | ------- |
| （分段名） | `name` | 必填 | 监听器名，在 `listeners` 内唯一（精确字符串匹配）。 |
| `type` | `listener_type` | `mixed` | 监听器协议。只接受 `mixed`，其他值均为解析错误。 |
| `listen` | `listen` | `127.0.0.1` | 绑定的本地 IP。显式设置时允许 `0.0.0.0`。 |
| `port` | `port` | 必填 | 本地 TCP 端口，1..=65535。`0` 会被拒绝。 |
| `proxy` | `proxy` | `null` | 出站固定目标：节点名、组名、`direct` 或 `block`。留空则走全局路由。 |

监听器名与代理名按原样保留——中文与 emoji 精确保留，不做归一化或
ASCII 折叠。两个监听器只有在监听地址不同时才能共用一个端口；相同的
`(listen, port)` 会被拒绝，重名也会被拒绝。

## 选择代理目标

> **警告：不要把监听器指向 `score`、`urltest` 或 `loadbalance` 组。**
> 这些策略可能在会话中途切换所选节点，导致出口 IP 变化，破坏指纹
> 一致性（会出现登录验证、验证码或封禁）。请把每个监听器固定到
> **单个节点**，或固定到 `policy: fixed(0)` 的组（或操作者从不手动切换
> 的 Selector 组）。

`proxy` 必须命名一个已存在的节点、组、`direct` 或 `block`，否则校验
失败。`direct` 表示该监听器的流量直连出站；`block` 表示丢弃。

## 说明

- `listen` 默认只监听回环地址。需要向局域网暴露时，请显式设置
  `listen: 0.0.0.0`（或局域网地址）；监听器没有认证，仅在可信网络
  中这样做。
- 监听器变更需要重启：修改 `listeners { }` 后发送 SIGHUP 不会重新绑定端口。
- 监听器端口不得与 `global.tproxy_port`（默认 12345）、`dns.bind` 或
  Clash `external_controller` 端口冲突。
