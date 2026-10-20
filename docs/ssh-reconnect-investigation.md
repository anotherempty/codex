# Shared app-server reconnect bridge

Related: [openai/codex#41849](https://github.com/openai/codex/issues/41849).

## Problem

After a VS Code Remote-SSH disconnect, an old extension host can remain alive
with its private Codex app-server. A replacement extension host starts another
server, which cannot resume the thread because the original server still owns
its writer lock. The lock is doing its job: deleting it or killing an active
owner is not a safe reconnect mechanism.

A shared app-server already supports resuming a loaded thread from another
connection, including its active turn and pending approvals. This branch adds a
transport adapter so a newline-JSON IDE client can use that capability.

## Implementation

```sh
codex app-server proxy --json-rpc --sock /absolute/path/to/server.sock
```

Without `--sock`, the CLI uses the existing canonical control-socket discovery.
The standalone adapter can be built without compiling the entire CLI:

```sh
cd codex-rs
cargo build -p codex-stdio-to-uds
./target/debug/codex-stdio-to-uds --json-rpc /absolute/path/to/server.sock
```

The adapter performs the WebSocket handshake and forwards JSON-RPC in both
directions. IDs and message contents are retained; server JSON is compacted to
one stdout line. It supports server requests (including approvals),
notifications, ping/pong, bounded input lines, and independent bidirectional
backpressure. Stdin EOF detaches only that client. Backend failure returns an
error without waiting for the IDE to close stdin. There is no automatic reconnect
inside the adapter: the IDE must initialize a fresh connection and resume its
thread, rather than risk replaying non-idempotent requests.

Existing byte-proxy behavior is unchanged when `--json-rpc` is absent. The bridge
does not start a daemon, change configuration, remove locks, or fall back to a
private server. It connects to an explicitly provisioned compatible backend.

For an isolated development backend, a separate process can run:

```sh
codex app-server --listen unix:///absolute/private/directory/server.sock
```

The existing managed-daemon lifecycle can instead provide a persistent backend.
Its configuration and version must be selected deliberately. Shared clients use
the backend's environment; per-client environment isolation is not provided.
Do not assume launch-time CLI overrides apply through a proxy.

## Regression checks

```sh
cd codex-rs
cargo test -p codex-stdio-to-uds
cargo clippy -p codex-stdio-to-uds --all-targets -- -D warnings
cargo check -p codex-cli --locked
```

The crate tests cover request/approval/notification forwarding, multiline JSON,
ping/pong without client traffic, invalid input, unsupported binary frames,
simultaneous large messages, backward-compatible byte forwarding, and a real
proxy process exiting when the backend closes while stdin remains open.

A credential-free integration regression uses a temporary Codex home and a
loopback mock model. Run from the repository root:

```sh
python3 scripts/test_shared_app_server_reconnect.py \
  --codex /absolute/path/to/codex \
  --bridge codex-rs/target/debug/codex-stdio-to-uds
```

On Linux, with the built bridge and the app-server bundled in extension
`26.1007.21434`, this verified:

- Another client resumes an active thread while the old client stays connected.
- A separate server reproduces the original writer conflict.
- Abrupt client loss followed by five reconnects preserves the original turn.
- That original turn completes after reconnect.
- A pending command approval is replayed with the same request ID and can be
  answered by the new client. The test declines it, so no command executes.
- Closing bridge stdin exits successfully without stopping the shared server.

The integration test uses an installed backend, not a freshly built fork
backend. No user configuration or existing conversation is changed.

## Remaining product integration

This is a working fork-side transport fix, not an automatic change to all IDE
installations. The public repository does not contain the VS Code extension
source. The extension must select/reuse the compatible backend and launch this
adapter (or connect natively through WebSocket). Its current private-server
launch behavior is not changed by building this branch.

Required follow-up includes backend startup/version negotiation, preserving
executor/configuration boundaries, safe migration of existing private writers,
actual IDE sleep/wake and SSH-drop testing, and cleanup of abandoned extension
hosts. A shared server prevents those hosts from creating competing writers but
does not itself reclaim their resources. Windows and macOS runtime validation
are still needed; the bridge uses the existing cross-platform `codex-uds` crate.

No upstream issue comment or pull request has been published.

## TUI reconnect follow-up

The interactive CLI already has native shared-daemon discovery and reconnection;
it does not need the stdio bridge. Resume/fork and picker paths already disable
embedded fallback after a backend has been selected.

A targeted regression exposed a separate recovery bug: `thread/resume` returns
`-32600` for both unavailable threads and active writer conflicts. Reconnect
previously treated the conflict as a successfully recovered but unavailable
conversation. It stopped retrying even if the original writer was about to
release ownership.

The TUI now retries active-writer conflicts for the selected conversation within
its existing retry budget. It preserves drafts, queued input, and backend
identity. A writer that never releases ownership leaves the client offline when
the budget expires; no writer is killed, no lock is deleted, and no replacement
thread is created. Command-center overview recovery can still succeed without
its optional selected conversation.

Regression cases cover a temporary conflict with queued and edited input, plus
an exhausted retry budget with embedded fallback nominally enabled. The latter
asserts that no `thread/start` or `turn/start` is sent.

Run the relevant TUI checks with enough stack for the large existing test futures:

```sh
cd codex-rs
RUST_MIN_STACK=16777216 cargo test -p codex-tui --lib reconnect
RUST_MIN_STACK=16777216 cargo test -p codex-tui --lib fallback
```

This does not change `codex exec` or either SDK. `exec` owns an embedded server and
its cancellation contract; TypeScript wraps `exec`, while Python owns a stdio
server. Shared-backend modes for those clients need separate lifecycle design.

Validation on Linux: both new conflict regressions failed before the TUI patch;
afterward all 24 reconnect tests and 78 fallback-filtered tests passed. The
initial default-stack test run aborted in an existing large test future; the
commands above use a 16 MiB test-thread stack and completed successfully.

`cargo clippy --no-deps -p codex-tui --lib --tests -- -D warnings` and formatting
checks also passed. Dependency-inclusive strict Clippy is blocked by an existing
`clippy::let_and_return` warning in unchanged
`app-server/src/message_processor.rs:382`; that unrelated source was not edited.
