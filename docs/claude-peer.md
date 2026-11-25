# graphlink-claude: Claude Code as a seat's model

This page says what the code does now. Not verified: **nothing has run against a real `claude` CLI or the Anthropic API.** Every test uses `graphlink-claude/tests/fake_claude.py`, a script that prints the stream-json shapes the seat's own claude rung (`packs/chat_turn.c1f` in the babyseat repo) reads and speaks MCP to the tool server the way Claude Code is expected to.

## What it is

A GraphLink peer, in the contract of the local model peer membrama (`src/peer.rs`, `src/turn.rs`), for a seat's chat rung (`packs/chat_membrama.c1f`, and the generic peer rung of babyseat#1205). The seat makes the control rings `/gl-<name>-ctl` and its cell table `/gl-<name>-sc` and starts `graphlink-claude serve` from its `peers` fact; the peer attaches them, makes `/gl-<name>-pc` (it is its only writer) and subscribes to `membrane::LlmRequest:`, `LlmMsg:` and `LlmTool:`. There is no loop of its own: only the seat's request, the tool, the turn, the response.

`src/peer.rs` is membrama's peer copied down to what this one uses; it should move into a shared crate and be used by both.

## The cycle

The seat writes `membrane::LlmTool:door:<i>.spec` (OpenAI function specs), `membrane::LlmMsg:<mid>:<i>` {role, content, tool_calls?} and last `membrane::LlmRequest:<mid>-<n>` {server, messages, tools, settings}. `settings` is not read (the seat may write it empty); `max_new` and `enable_thinking` play no part. The peer answers with `<ns>::Reply:<rid>` and `<ns>::Chunk:<rid>:<k>` (`rid` is `<mid>-<n>`):

- `Reply.state` is `running`, then `done`, `error` or `aborted` (written last, after the rest). `chunks` says how many chunk nodes exist; `Chunk.text` holds the round's text, each at most `val_cap - 3` bytes (the CBOR header), cut on a character; the seat reads up to four, so a longer text is an `error` that says so.
- `finish` is `stop`, or `tool_calls` with `tool_calls` `[{name, arguments}]` (bare tool names, the seat's own).
- `usage` is `{read, loaded, generated}`, summed over the round's assistant messages: `read` is `input_tokens + cache_creation_input_tokens` (what the model read fresh), `loaded` is `cache_read_input_tokens` (served from the prompt cache), `generated` is `output_tokens`. A key whose count the CLI did not give is left out, and with no usage at all there is no `usage` cell. The CLI's final `result` usage is not used: it is the whole run's, not the round's.
- The namespace is the `--ns` the owner gives: the seat must read `<ns>::Reply:` for it (`packs/chat_membrama.c1f` reads the literal `membrama::Reply:`; the generic peer rung reads the entry's `ns`).

## The model: one child per turn, a held tool call

Round 1 starts one `claude` child for the turn:

```
claude -p --input-format stream-json --output-format stream-json --verbose --tools '' --strict-mcp-config --mcp-config <json> \
       --allowedTools 'mcp__seat__*' --setting-sources '' --no-session-persistence --model <alias> --system-prompt <the system messages of round 1>
```

These are the flags of the seat's claude rung, less `--include-partial-messages` (the peer reads whole messages, not deltas) and `--max-turns` (the seat ends a turn by not writing the next round), plus `--model`. Claude's built-in tools are off. The user message of round 1 goes on stdin as one stream-json line and the pipe is closed. `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` is set, as the rung does.

The seat's tools reach the child as a stdio MCP server named `seat`: `graphlink-claude mcp --socket <path>`, which Claude spawns, linked to the peer by a unix socket in a folder (mode 0700) under the temporary directory. No network, no door call. When Claude calls a tool the shim HOLDS the call (no timeout of its own). The peer ends the round with the FIRST call the shim holds: `finish` is `tool_calls` and the Reply carries that one call. The CLI calls MCP tools one at a time (measured on the real CLI, claude 2.1.327: with two `tool_use` blocks in one message the second `tools/call` came only after the first was answered, with or without `readOnlyHint` on the tools), so waiting for every announced call waited for ever; a step with two tool calls is two rounds. A call that does come while the round has already ended is answered at once with an error that tells the model to call it again after the first result. The seat runs them and writes round n+1, whose last messages are the tool results; the peer gives result i to held call i (the count must match, one result per held call: one, else an `error` reply) and the child goes on; its next text or tool call is Reply n+1. The history of earlier rounds is not sent again: the child has it. A round n+1 for a turn this peer has no run for (it was restarted) is an `error` reply, not a guess.

**A tool result is capped on its way to the child** (`graphlink-claude/src/cap.rs`, #1143, operator 2026-11-24): the Claude CLI refuses a tool result over its own limit and the model then gets nothing, so the limit is kept here, where the result is handed to the held call, and not in the seat's door. A result of at most twice 8192 characters comes whole; a longer one is its first 8192 and last 8192 characters with a line between them that says how many characters were left out and how long the whole is. Characters are counted, not bytes. The seat keeps the whole result; only the copy for the CLI is cut. 8192 is the number the seat's door used before (`mc_w_cap`), moved here; it is not measured against the CLI's own limit. Tests: `graphlink-claude/tests/cap.rs`, `tests/turn.rs` (`a_tool_result_over_the_cap_reaches_the_child_as_its_head_and_tail`).

Built: the held call. The stateless replay fallback is **not** built.

## Ending

`abort` set on the current request, the retire of the request a run is on, and the seat's end of the rings going (or `GRAPHLINK_PARENT`, else the starting process, with `--die-with-parent`) all end the child with SIGTERM, never SIGKILL, and it is reaped; after `--term-wait-ms` a child still there is left and said on stderr. When the seat retires a request, its Reply and Chunk nodes are retired.

A child that exits non-zero, or ends without a result line, is an `error` reply that carries the end of its stderr (2000 bytes); a `result` line with `is_error` is an `error` reply. The API key, if it appears in an error text, is replaced by `[key]`.

## Authentication

`ANTHROPIC_API_KEY` comes from this process's environment and reaches the child through its environment only. `serve` refuses to start without it. In key mode `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`/`VERTEX`/`FOUNDRY` and `CLAUDE_CODE_OAUTH_TOKEN` are removed from the child's environment (the list `packs/chat_turn.c1f` uses). `--auth login` is the CLI's own login: the key and the credentials that outrank the login are removed, `CLAUDE_CODE_OAUTH_TOKEN` stays. It is opt-in and never chosen silently.

## Numbers

All required on the command line, no default: `--slots`, `--key-cap`, `--val-cap`, `--epoch`, `--spins`, `--depth` (the regions and their reading, equal to the seat's `peers` fact), `--nap-ms` (the loop's nap), `--term-wait-ms` (how long to wait for a child after SIGTERM), and `--model` (the alias). There is no blocked-tool-call timeout and no `--max-turns`. Constants in the code, not options: four chunks (what the seat reads) and 2000 bytes of stderr in an error.

## Not verified

- The real CLI's behaviour beyond the one measured fact above (tool calls come one at a time): that `claude -p` with these flags starts the MCP server from `--mcp-config`, lists the tools, holds a call for as long as it takes (the measured test held one for 15 s), and prints `assistant` lines with `tool_use` blocks before it calls (formerly the round ended on counting them: a CLI that prints them after the call, or one block per line late, would end a round early or late).
- That a live `result` line, the usage fields and the `is_error` shape are what the fake prints.
- That a SIGTERM to the child ends its MCP server too (the shim also ends when its stdin or the socket closes).
- A socket path over 100 bytes (a long `TMPDIR`) is refused with a message.


## The models it serves

Once the link is open the peer publishes ONE node of its own, `<ns>::Models:main`:

- `names` = `["claude", "sonnet", "opus", "haiku"]`: the models it answers;
- `backend` = `"claude-peer"`: what a seat files them under.

A seat that reads this node routes those names to the peer while its link is up, with no `cloud_models` entry per model (membrane: `packs/chat_membrama.c1f`, `mb_entry`).

A request names its model as `membrane::LlmRequest:<rid>.model` (round 1 decides; later rounds continue the same child):

| `model` | the child runs with |
|---|---|
| none, or `claude` | `--model <the owner's --model>` (as before this field existed) |
| `sonnet`, `opus`, `haiku` | `--model <that alias>` |
| anything else | nothing starts; the reply is `error`: `this peer does not serve the model <m>: it serves claude, sonnet, opus, haiku` |

Tests: `graphlink-claude/tests/models.rs` (the fake CLI, not the real one).
