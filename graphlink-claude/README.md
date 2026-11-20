# graphlink-claude

Claude Code (the `claude` CLI in print mode, `-p`) as a model backend of a GraphLink seat. It joins the seat over shared memory as an ordinary peer, the way a local model peer does, and answers the seat's chat requests: request, tool, turn, response. There is no loop of its own.

```
graphlink-claude serve --peer <name> --ns <namespace> --claude-bin <path> --model <alias> \
    --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> --term-wait-ms <n> \
    [--auth api_key|login] [--token-file <path>] [--die-with-parent]
graphlink-claude mcp --socket <path>        # spawned by Claude Code, not by hand
```

Every number is the owner's and has no default; `--model`, `--claude-bin`, `--peer` and `--ns` are required too. The numbers of the regions must equal what the seat's `peers` fact declares. The full page, what is verified and what is not: [docs/claude-peer.md](../docs/claude-peer.md).

**Authentication.** The API key is the environment variable `ANTHROPIC_API_KEY` of this process (never an option, never in a cell, argv or log); without it `serve` refuses to start. The CLI's own login is used only with `--auth login`, never chosen silently.

**Not verified:** nothing here has run against a real `claude`; the tests use a fake one (`tests/fake_claude.py`).

Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
