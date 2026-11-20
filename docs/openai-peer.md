# graphlink-openai: an OpenAI-compatible server as a seat's model

This page says what the code does now. Not verified: **nothing has run against a real OpenAI-compatible server.** Every test uses a fake server (`graphlink-openai/tests/common/mod.rs`, a `TcpListener` that sends the shapes the Chat Completions streaming API is documented to send) and the real `curl` against it on 127.0.0.1.

## What it is

A GraphLink peer, in the contract of the local model peer membrama and of [graphlink-claude](claude-peer.md), for a seat's chat rung (`packs/chat_membrama.c1f` and the generic peer rung of babyseat#1205). The seat makes the control rings `/gl-<name>-ctl` and its cell table `/gl-<name>-sc` and starts `graphlink-openai serve` from its `peers` fact; the peer attaches them, makes `/gl-<name>-pc` (it is its only writer) and subscribes to `membrane::LlmRequest:`, `LlmMsg:` and `LlmTool:`. There is no loop of its own: only the seat's request, the tool, the turn, the response.

`src/peer.rs` is graphlink-claude's peer (which is membrama's), copied: the same file in three places. It should move into a shared crate and be used by all of them.

## The cycle

The seat writes `membrane::LlmTool:door:<i>.spec` (OpenAI function specs), `membrane::LlmMsg:<mid>:<i>` {role, content, tool_calls?} and last `membrane::LlmRequest:<mid>-<n>` {messages, tools, settings}. The peer answers with `<ns>::Reply:<rid>` and `<ns>::Chunk:<rid>:<k>` (`rid` is `<mid>-<n>`), exactly as graphlink-claude does:

- `Reply.state` is `running`, then `done`, `error` or `aborted` (written last, after the rest). `chunks` says how many chunk nodes exist; `Chunk.text` holds the round's text, each at most `val_cap - 3` bytes (the CBOR header), cut on a character, rewritten as the text grows; the seat reads up to four, so a longer text is an `error` that says so and the call is ended.
- `finish` is `tool_calls` (with `tool_calls` `[{name, arguments}]`, the arguments a JSON value) when the model called tools, else `stop`; a reply the server cut (`finish_reason` `length` or `content_filter`) says that word in `finish` instead of passing for complete.
- `usage` is `{read, loaded, generated}`: `read` is `prompt_tokens` less the cached part, `loaded` is `prompt_tokens_details.cached_tokens`, `generated` is `completion_tokens`. A key the server did not give is left out; with no usage at all there is no `usage` cell.

## The call: one `curl` per request

The API keeps nothing, so there are no held children and no tool shim: **every request is one call** to `<base-url>/chat/completions` that carries the whole history. Round n+1 is a new call whose messages are round n's plus the assistant message with its calls and the tool results; its reply is Reply n+1. Nothing is lost by restarting this peer between rounds.

The body is `{model, messages, stream: true, stream_options: {include_usage: true}, tools?, max_tokens?}`. `tools` is the seat's specs as written, left out when there are none. `max_tokens` is the request's `settings.max_new` when the seat wrote one, else absent (no number of this peer's own). The seat's assistant messages carry their calls as `{type, function: {name, arguments}}` with no ids and its tool messages no `tool_call_id`, and the API wants both: **the peer makes the ids** (`call_<message index>_<call index>`) and each `tool` message takes the next call not yet answered, in order. A tool message with no call waiting for it is an `error` reply and no call is made.

The answer is read as a stream of server-sent events: `delta.content` is the text, `delta.tool_calls` fragments are joined by index (name, arguments' JSON text), `finish_reason` and a final `usage` object are kept, and `[DONE]` ends it. Arguments that are not JSON are an `error` reply, not a guess. A server that ignores `stream: true` and answers with one JSON object is an `error` reply that carries the start of what it said: the stream is required, a non-streaming fallback is **not** built.

A status that is not 2xx is an `error` reply, `the server answered HTTP <status>: <error.message or the body>` (2000 bytes at most). A curl that fails (no connection, bad certificate) is an `error` reply with the end of its stderr. An `error` object in the stream is an `error` reply. The API key, if it appears in an error text, is replaced by `[key]`.

## The transport: the system `curl`

No HTTP library is built in (nothing new to compile or fetch), like graphlink-claude shells out to `claude`. `--curl-bin` names the executable. **The key and the headers are in curl's configuration, written to its stdin (`-K -`), never in its argv** (which any process on the machine can read); the argv is only `--silent --show-error --no-buffer -K -`. The body is in a file in a folder only this user can open (mode 0700, the file 0600), removed when the call ends. Each value in the configuration is quoted and escaped, so a value cannot end its line and start another. The status comes from curl's `--write-out` line after the body (`glstatus:<code>`; `000` is no answer at all). TLS, proxies and certificates are curl's own.

## Ending

`abort` set on the request a call is on, the retire of that request, and the seat's end of the rings going (or `GRAPHLINK_PARENT`, else the starting process, with `--die-with-parent`) all end the call with SIGTERM to its curl, never SIGKILL; the curl is reaped, and after `--term-wait-ms` one still there is left and said on stderr. When the seat retires a request, its Reply and Chunk nodes are retired. There is no timeout of the call's own: a server that never answers holds the call until the seat aborts it.

## Authentication

`--key-env <VAR>` names the environment variable that holds the API key; it is read when a call starts and reaches curl through its stdin only. `serve` refuses to start when the variable is not set or empty, naming the variable and never a value. Without `--key-env` no `Authorization` header is sent, which is the owner's explicit choice for a server that wants none (a local one); it is never the result of a missing variable.

## Numbers

All required on the command line, no default: `--slots`, `--key-cap`, `--val-cap`, `--epoch`, `--spins`, `--depth` (the regions and their reading, equal to the seat's `peers` fact), `--nap-ms` (the loop's nap), `--term-wait-ms` (how long to wait for a curl after SIGTERM), and `--base-url`, `--model`, `--curl-bin`. Constants in the code, not options: four chunks (what the seat reads) and 2000 bytes of an error's text.

## Not verified

- Any real server: that the fields read here (`choices[0].delta.content`, `delta.tool_calls[].index/function`, `finish_reason`, `usage.prompt_tokens`, `prompt_tokens_details.cached_tokens`, `completion_tokens`) are what OpenAI, vLLM, llama.cpp or Ollama really send, that `stream_options` is accepted by each of them, and that every one sends the final `usage` chunk.
- Servers that send several `choices`, parallel tool calls with out-of-order `index`es, or a tool call whose `index` is missing (it is read as 0).
- That a tool result message following an assistant message with calls always answers them in the order of the calls.
- curl's behaviour across versions: that `-K -` with `write-out`, `data-binary @file` and `header = "Expect:"` behave the same in the curl of the machine this runs on as in the one the tests ran with.
