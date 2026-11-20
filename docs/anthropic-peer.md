# graphlink-anthropic: the Anthropic Messages API as a seat's model

This page says what the code does now. Not verified: **nothing has run against the real Anthropic API.** Every test uses `graphlink-anthropic/tests/common/mod.rs` (`FakeApi`), a std TCP server on 127.0.0.1 that prints the streaming events the API's documentation describes; the HTTP itself is the machine's real `curl`.

## What it is

A GraphLink peer in the same contract as `graphlink-claude` ([claude-peer.md](claude-peer.md)) and the local model peer membrama, for a seat's generic peer rung (`packs/chat_membrama.c1f`). The seat makes the control rings `/gl-<name>-ctl` and its cell table `/gl-<name>-sc` and starts `graphlink-anthropic serve` from its `peers` fact; the peer attaches them, makes `/gl-<name>-pc` and subscribes to `membrane::LlmRequest:`, `LlmMsg:` and `LlmTool:`. There is no loop of its own: only the seat's request, the tool, the turn, the response.

`src/peer.rs` is `graphlink-claude`'s peer copied (which was membrama's): **three copies now; it should move into a shared crate** and be used by all of them. The turn handler is its own, because this backend is stateless.

## The cycle

The seat writes `membrane::LlmTool:door:<i>.spec` (OpenAI function specs), `membrane::LlmMsg:<mid>:<i>` {role, content, tool_calls?} and last `membrane::LlmRequest:<mid>-<n>` {messages, tools, settings}. `settings` is not read. The peer answers with `<ns>::Reply:<rid>` and `<ns>::Chunk:<rid>:<k>`:

- `Reply.state` is `running`, then `done`, `error` or `aborted` (written last). `chunks` says how many chunk nodes exist; `Chunk.text` holds the round's text, each at most `val_cap - 3` bytes; the seat reads up to four, so a longer text is an `error` that says so. The text grows chunk by chunk while the call streams.
- `finish` is `stop`, `length` (the API stopped at `max_tokens`) or `tool_calls` with `tool_calls` `[{name, arguments}]` (arguments an object).
- `usage` is `{read, loaded, generated}`: `read` is `input_tokens + cache_creation_input_tokens`, `loaded` is `cache_read_input_tokens`, `generated` is `output_tokens` (the count `message_delta` ends with). A key whose count the API did not give is left out.

## A round is one call

Every request is one `POST <base-url>/v1/messages` with `stream: true`. The API keeps no history, so the body is built from the whole conversation the request names (`src/wire.rs`):

- `model` and `max_tokens` are the owner's options; the system messages joined by a blank line are `system`; `tools` is the specs as `{name, description, input_schema}` (none, none sent).
- a user message is a text block; an assistant message is a text block (when it has text) plus one `tool_use` block per call in `tool_calls` (arguments an object, or JSON text that parses to one); its id is made here (`toolu_<message index>_<k>`).
- the seat's `tool` messages carry no call id: they answer the calls of the assistant message before them in order, and become `tool_result` blocks of ONE user message with the id of the call they answer. A count that does not match is an `error` reply and no call is made.

## The call is a `curl` child

No new dependency: `curl -sS -N -K -`. The key and the headers (`x-api-key`, `anthropic-version`, `content-type`, `accept`), the URL and the body file go to curl as a config on its standard input, so **the key is never on a command line**; the body is a file in a folder (mode 0700) under the temporary directory, removed when the call ends. The stream is read line by line: `message_start`, `content_block_start/delta` (`text_delta`, `input_json_delta`), `message_delta` (stop reason, output tokens), `message_stop`, and `error`. A body that is not a stream (an HTTP error answers with a JSON body) is an `error` reply with the API's `error.type` and `error.message`; curl failing (no server, refused) is an `error` reply with the end of its stderr (2000 bytes). The key, if it is in an error text, is replaced by `[key]`.

## Ending

`abort` set on the request, the retire of the request a call is on, and the seat's end of the rings going (or `GRAPHLINK_PARENT`, else the starting process, with `--die-with-parent`) end the curl with SIGTERM, never SIGKILL, and it is reaped; after `--term-wait-ms` one still there is left and said on stderr. When the seat retires a request, its Reply and Chunk nodes are retired.

## Authentication and numbers

The key is the environment variable that `--key-env` NAMES, read from this process's environment; `serve` refuses to start when it is empty. All required, no default: `--peer`, `--ns`, `--curl-bin`, `--base-url`, `--model`, `--max-tokens` (at least 1), `--anthropic-version`, `--key-env`, `--slots`, `--key-cap`, `--val-cap`, `--epoch`, `--spins`, `--depth`, `--nap-ms`, `--term-wait-ms`. There is no timeout of the call: the seat ends a turn. Constants in the code, not options: four chunks (what the seat reads) and 2000 bytes of stderr or error body in an error.

## Not verified

- Everything against the real API: that the streaming events have the shapes the fake prints (a tool call's arguments arrive as `input_json_delta` pieces; usage in `message_start` and `message_delta`), that `anthropic-version` and the headers are what it accepts, that a `thinking` block (ignored here) never needs to be sent back.
- That the seat's rung accepts the finish value `length` (the others are the ones the other peers write).
- `curl`'s behaviour on other platforms than the one the tests ran on; a call is only as good as the machine's `curl` and its TLS.
- A very large conversation: the body goes through a file, but the seat's table limits what a message can be.
