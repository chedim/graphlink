# graphlink-bridge

TCP outward, shared memory inward. It joins a program that has no socket (a GraphLink seat) to a remote peer: to the seat the bridge is an ordinary local peer; to the remote it is a TCP peer.

```
graphlink-bridge connect <addr> --peer <name> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n>
graphlink-bridge listen  <addr> ...the same... [--allow-remote]
```

- **`--peer <name>`** is the peer the seat declared: the seat makes `/gl-<name>-ctl` (control rings) and `/gl-<name>-sc` (its cell table); the bridge makes `/gl-<name>-pc` (the remote's values, as cells). The numbers are the owner's; there are no defaults.
- What the seat asks (`sub`, `unsub`, `read`, `query`, `wr`) goes to the remote; the remote's values become cells; receipts and pages come back over the rings. A node outside the remote's namespace is dropped both ways.
- **`--seat-name <name>`**: what the seat is called across the link, for two seats that are both `membrane`.
- **Tokens:** `--token-file` (put in the hello to the seat), `--remote-token-file` (put in the hello to the remote), `--secret-file` (check every remote's hello; required with `--allow-remote`).
- **TLS:** `listen --tls-dir <dir>` makes a self-signed certificate once; `connect --tls-cert <file>` trusts exactly that file.
- **`--die-with-parent`** ends the bridge with the process that supervises it (named by `GRAPHLINK_PARENT`).

The program is also a library (`graphlink_bridge::run()`), so a host can ship the same binary. Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
