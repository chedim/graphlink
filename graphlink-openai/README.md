# graphlink-openai

Any OpenAI-compatible chat completions server (`<base-url>/chat/completions`) as a model backend of a GraphLink seat. It joins the seat over shared memory as an ordinary peer, the way a local model peer does, and answers the seat's chat requests: request, tool, turn, response. There is no loop of its own. The call is made by the system `curl` (no HTTP library).

```
graphlink-openai serve --peer <name> --ns <namespace> --curl-bin <path> --base-url <url> --model <name> [--key-env <VAR>] \
    --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> --term-wait-ms <n> \
    [--token-file <path>] [--die-with-parent]
```

Every number is the owner's and has no default; `--curl-bin`, `--base-url`, `--model`, `--peer` and `--ns` are required too. The numbers of the regions must equal what the seat's `peers` fact declares. The full page, what is verified and what is not: [docs/openai-peer.md](../docs/openai-peer.md).

**Authentication.** The API key is read from the environment variable that `--key-env` names (never an option, never in a cell, argv or log; `curl` gets it on its stdin); `serve` refuses to start when the variable is not set. Without `--key-env` no `Authorization` header is sent, for a server that wants none.

**Not verified:** nothing here has run against a real OpenAI-compatible server; the tests use a fake one.

Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
