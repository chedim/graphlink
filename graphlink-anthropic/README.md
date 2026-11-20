# graphlink-anthropic

The Anthropic Messages API, called directly (no `claude` CLI), as a model backend of a GraphLink seat. It joins the seat over shared memory as an ordinary peer, the way a local model peer does, and answers the seat's chat requests: request, tool, turn, response. There is no loop of its own.

```
ANTHROPIC_API_KEY=... graphlink-anthropic serve --peer <name> --ns <namespace> --curl-bin <path> --base-url <url> --model <name> \
    --max-tokens <n> --anthropic-version <v> --key-env <VAR> \
    --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> --term-wait-ms <n> \
    [--token-file <path>] [--die-with-parent]
```

Every option and number is the owner's and has no default (`--max-tokens` is required by the API on every call). The numbers of the regions must equal what the seat's `peers` fact declares. The full page, what is verified and what is not: [docs/anthropic-peer.md](../docs/anthropic-peer.md).

**The key** is never an option: `--key-env` names the environment variable of this process that holds it. It reaches `curl` on its standard input, never on its command line; it is masked in error replies.

**Not verified:** nothing here has run against the real API; the tests use a fake server on 127.0.0.1 (and the machine's real `curl`).

Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
