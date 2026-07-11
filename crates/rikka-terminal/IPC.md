# RikkaTerminal IPC (P2)

One local IPC shared by three binaries:

| Binary | Role | IPC | Platforms |
|--------|------|-----|-----------|
| `rikka-terminal` | window process + monarch (first one) | server | all |
| `rt` | thin launcher → `spawn` | client | all |
| `rikka-handoff` | Windows default-terminal shim → `attach` | client | Windows only |

## Goals

- **Cross-platform.** The launcher path (`spawn`) works on Linux/macOS/Windows.
  The OS default-terminal handoff (`attach` from `rikka-handoff`) is Windows-only.
- **Crash isolation (the WT pain point we're fixing).** In Windows Terminal a
  crash inside one window takes the whole app down. Here every window is its own
  OS process, so a crash in window A cannot reach window B. The monarch is a
  coordinator, never a cascade point: if it dies, other window processes keep
  running (coordination just pauses until re-election).
- **Flat topology.** A client sends one message (or starts main once) and exits.
  No launcher chains.

## Architecture — monarch + independent window processes

- **Window process** = one `rikka-terminal` process = one OS window and its
  tabs. It solely owns its PTYs, its `alacritty` `Term`s, and its gpui window.
  **No shared mutable state** with other window processes — that is what makes
  the isolation real.
- **Monarch** = the first process to bind the socket. Coordinator only:
  single-instance election, socket ownership, the window registry, and routing.
  It also hosts its own window (it is itself a window process).
- **Isolation guarantee:**
  - window A crashes → only A's tabs die; every other window survives.
  - monarch crashes → its own window dies; other windows keep running
    (separate processes); new spawns pause until a monarch is re-elected (v2).
  - **Never a whole-app cascade.**

## Transport

- [`interprocess::local_socket`](https://docs.rs/interprocess): a Unix-domain
  socket on Unix, a named pipe on Windows — one Rust API. (Windows also supports
  AF_UNIX since 1803, but named pipes give better ACLs and no stale socket file;
  the handle transfer is out-of-band `DuplicateHandle` either way, so we do not
  need `SCM_RIGHTS`-style in-band passing on Windows.)
- **Endpoint (per user):**
  - Linux: `$XDG_RUNTIME_DIR/rikka-terminal.sock` (fallback `/tmp/rikka-terminal-$UID.sock`)
  - macOS: `$TMPDIR/rikka-terminal.sock`
  - Windows: `\\.\pipe\rikka-terminal.<SID>`
- **Framing:** `[u32 LE byte-length][UTF-8 JSON]`, one request → one response.
- **Election:** first process to create the endpoint = monarch; others become
  clients (forward their request, exit). Windows: `FILE_FLAG_FIRST_PIPE_INSTANCE`.
  Unix: exclusive bind (unlink stale socket first, guarded by a lock file).
- **Security:** current user only (pipe ACL restricted to the creator SID; on
  Unix the socket lives in a 0700 user dir).

## Protocol (JSON, versioned)

```
Request:  { "v":1, "op":"…", … }
Response: { "v":1, "ok":true, … }   |   { "v":1, "ok":false, "error":"…" }
```

Unknown `op` or `v` → error response. `ping` is liveness/handshake.

### `spawn` — launcher (all platforms)

```json
{ "v":1, "op":"spawn",
  "cwd":"…", "argv":["pwsh","-l"], "profile":"pwsh",
  "title":null, "hold":false,
  "target":"new" | {"window":<id>} }
```

Strings only. `target:"new"` → monarch spawns a new window process.
`target:"window:<id>"` → route to that window process → open a new tab there.

### `attach` — handoff + tab-move

```json
{ "v":1, "op":"attach",
  "pid":<sender pid>,
  "handles":{ "input":…, "output":…, "signal":…, "reference":…, "server":…, "client":… },
  "startup":{ "title":…, "x":…, "y":…, "cols":…, "rows":… },
  "state":<optional serialized grid+scrollback — tab-move only>,
  "elevated":false,
  "target":"new" | {"window":<id>} }
```

- **Who creates the VT pipes (OS handoff):** `ITerminalHandoff3` made `in`/
  `out` **[out] params** — the *terminal* creates the pipe pair (so it owns
  buffering choices) and returns the console's ends from the COM call;
  `signal`/`reference`/`server`/`client` remain console-created `[in]`
  handles. `handles.input`/`.output` in the message are therefore the shim's
  OWN pipe ends (terminal side), pulled by the monarch like every other
  handle.
- **Handle transfer** = out-of-band, receiver-pulls:
  `OpenProcess(pid, PROCESS_DUP_HANDLE)` then
  `DuplicateHandle(sender → me, DUPLICATE_CLOSE_SOURCE)` per handle (ownership
  moves; the sender's handles are closed by the dup). The sender waits for `ok`,
  then exits. On Unix the equivalent for tab-move is `SCM_RIGHTS` fd-passing over
  the socket — same message shape, transfer mechanism is `#[cfg(…)]`.
- `state` **absent** → OS handoff (a fresh PTY; there is no scrollback to carry).
  `state` **present** → cross-window tab-move (carry the grid/scrollback). v1 may
  ship without `state` (move the PTY, drop scrollback) and add it later.
- `target:"new"` (default for OS handoff) → new window process. `window:<id>` →
  drag-merge into an existing window as a tab.

### `register_window` / `list_windows` — window ↔ monarch

```json
{ "v":1, "op":"register_window", "pid":…, "window_id":…, "endpoint":"…" }
{ "v":1, "op":"list_windows" }  →  { "ok":true, "windows":[{ "id":…, "title":… }, …] }
```

Window processes register with the monarch so it can route `target:"window:<id>"`
and answer `-w` queries.

## One primitive, three uses (`attach`)

1. **OS default-terminal handoff** (Windows): `rikka-handoff` → `attach` (handles,
   no `state`, `target:new`) → new window.
2. **New window**: `spawn`/`attach` with `target:new` → monarch spawns a window
   process and hands it the content.
3. **Tab drag-merge across windows**: window A → `attach` (handles + `state`,
   `target:window:<B>`) → B adopts it as a tab; A drops it.

The mouse tab detach/merge and the OS handoff are **the same transfer**.

## Cold start (main not running)

The request rides *in the launch*, not over the socket — no wait-for-server race:

- **spawn cold:** `rt` spawns `rikka-terminal <args>`; the first instance handles
  the args and becomes monarch.
- **attach cold:** the shim does `CreateProcess(rikka-terminal --attach …,
  bInheritHandles=TRUE)`, passing the PTY handle values as args; the started main
  adopts the *inherited* handles (inheritance is the transfer — no
  `DuplicateHandle`) and becomes monarch.
- **Race:** two starts → both try to bind. Winner = monarch (handles its own
  request). Loser forwards its request over IPC (an attach loser
  `DuplicateHandle`s into the winner) and exits.

## Windowing rules

- `attach` (handoff) default `target:"new"` → new window process. **Never auto-tab.**
- `spawn` (launcher) default per wt `useNew` → new window process.
  `target:"window:<id>"` (explicit `-w <id>`) → a tab in that window.
- `elevated:true` (Windows) → monarch spawns an *elevated* window process (P3).
  v1 carries the flag only.

## Deferred

- Monarch re-election when the monarch process exits (v2). v1: monarch = first
  process; if it exits, coordination pauses until the next cold start.
- `state` wire format for tab-move scrollback (v1 may omit → move the PTY, drop
  scrollback).
- Elevated handoff window process (P3).

## Platform gating

- `attach`, handle transfer, and the default-terminal handoff are `#[cfg(windows)]`.
- Linux/macOS use `spawn` for the launcher and (later) `attach` + `SCM_RIGHTS`
  for tab-move. There is no OS default-terminal handoff outside Windows.
