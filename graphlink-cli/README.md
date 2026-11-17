# graphlink-cli

The `graphlink` tool: a demo peer and a debugging aid for [GraphLink](https://github.com/chedim/graphlink). Install it with `cargo install graphlink-cli`.

```
graphlink serve <addr> --ns <namespace> --nodes <file.json> [--secret-file <file>] [--tls-dir <dir>]
graphlink watch <addr> <id> | --prefix <prefix> [--token-file <file>] [--ns <namespace>] [--tls-cert <file>]
graphlink write <addr> <id> <prop> <json> [--token-file <file>] [--ns <namespace>] [--tls-cert <file>]
graphlink token mint --secret-file <file> --sub <sub> --ns <namespace> --ttl <seconds>
```

- **`serve`** owns the nodes of a JSON file (`{id: {prop: value}}`) and answers peers; with a secret every hello must carry a token for its namespace.
- **`watch`** prints the current value of a node (or every node under a prefix) as one JSON line, then each change.
- **`write`** changes one prop and prints `ok` or `refused: why` (exit 1 when refused).
- **`token mint`** prints a token that lets its holder own a namespace; the life is required, there is no default.

MIT licensed.
