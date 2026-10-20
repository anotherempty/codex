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

## Integration and ownership contract

The VS Code integration is blocked on the extension source, which is not in this
checkout. Its launch path must select a compatible shared backend before any
thread request, initialize each new connection, and resume the same thread after
transport loss. It must not retry an uncertain `turn/start` without reconciling
server state. The bridge is ready for that launch path; modifying an installed
minified extension would not deliver a repository fix for all installations.

A permanently live private owner cannot be distinguished from a legitimate
client using only its lock. The server cannot infer an SSH disconnect when the
extension host and its stdio connection remain alive. Automatic eviction would
require an explicit ownership/lease or cooperative handoff protocol, supported
by both clients and servers, including active turns and approvals. Lock deletion,
PID-age thresholds, and timeout-based killing are not valid replacements.
Migration must wait for the private owner to finish and exit before attaching
that thread to the shared backend. New clients should share the backend from
startup so a surviving extension host cannot become a competing writer.

SDK shared mode must be explicit. Launch configuration and environment belong to
the server, not the attaching process. Closing a connection detaches it; cancelling
a turn requires `turn/interrupt`. A lost connection is not permission to replay
mutating requests. A replacement client must initialize and resume the recorded
thread ID, inspecting its active turn before submitting further work.

## Python SDK shared attachment

`CodexConfig.app_server_socket` opts sync and async clients into the JSON-RPC
proxy. The default private stdio launch remains unchanged. The new option rejects
relative socket paths and conflicting launch/environment/configuration overrides.
Unversioned shared backends cannot be schema-probed through the proxy executable;
the existing feature-version checks fail closed instead of inspecting the wrong
runtime. Reconnect uses a new client instance; no uncertain requests are replayed.

Validation: 91 Python unit tests passed, plus Ruff checks. The credential-free
integration script now accepts `--python-sdk`; with SDK dependencies installed,
its ten checks cover both Python clients resuming the same live turn, detachment
without cancellation, and explicit interruption without stopping the daemon, in
addition to the existing bridge/approval regressions. It uses the installed
backend and a CLI-shaped launcher for the fork-built standalone bridge, not a
freshly built complete Codex executable. No real IDE network interruption or
Windows/macOS runtime check is implied by these Linux results.

## Experimental shared `exec` contract

`codex exec --app-server-socket /absolute/private/server.sock ...` explicitly
selects an existing local shared app-server. There is no discovery, startup,
private fallback, reconnect loop, or request replay. Local-socket transport keeps
file paths on the same execution host. The server must be compatible with this
CLI and already configured with the intended credentials and executor.

The initial mode supports new turns and `resume <thread-uuid>`. Named lookup,
`--last`, review, fork, managed worktrees, local providers, profile selection,
arbitrary CLI config overrides, and ignore-config/rules switches are rejected.
Those operations need additional shared-backend semantics rather than silent
reuse of local-only state. Caller workload identity and explicit credential,
endpoint, or executor environment variables are also rejected. The CLI still
loads local configuration for existing thread/turn parameters; the shared server
owns launch configuration and enforces its own restrictions. Provision both with
compatible settings; this does not transfer per-process environment.

Normal completion unsubscribes the client and closes its connection without
stopping the backend. A Unix SIGTERM (including a TypeScript AbortSignal) or
Ctrl-C requests interruption of the selected turn. Forced process termination
and loss of connectivity cannot guarantee cancellation; the backend may continue
work, and callers must inspect it through a fresh connection. Transport loss
returns failure and cannot be mistaken for a completed turn.

`exec resume` submits a new turn; it is not an active-turn event replay API. If the
thread already has an active turn, the server rejects another start. Clients that
need to inspect or reattach to that turn should use the app-server API (including
the Python shared mode) before submitting further work.

## TypeScript SDK shared attachment

`CodexOptions.appServerSocket` forwards an explicit socket to the new `exec` mode.
It rejects conflicting environment/config/credential options before spawning.
Windows shared mode is rejected because Node's forced child termination cannot
provide the Unix SIGTERM-to-turn-interrupt behavior. Defaults remain unchanged.

Validation on Linux: 69 exec library/binary tests, 16 existing private-backend
resume integration tests, scoped strict exec Clippy, and the CLI compile check
passed. The TypeScript SDK passed 17 exec unit tests, ESLint, declaration build,
and TypeScript checking (`--noEmit --rootDir . --allowImportingTsExtensions`,
which also accommodates the repository's existing sample imports).

The isolated regression script accepts `--exec /path/to/codex-exec` and
`--typescript-sdk` (build `sdk/typescript` first, with Node on PATH). All 16 checks
passed with the installed backend and fork-built bridge/exec: these include
normal shared exec completion, missing-socket failure without fallback, SIGTERM
interruption, backend-loss failure, public TypeScript SDK completion, and actual
AbortSignal cancellation verified against server-side turn state. The temporary
CLI-shaped launcher delegates to the built `codex-exec`; this validates the real
SDK/exec path without claiming the installed Codex CLI has the new flag.

## SSH and platform validation

Run `scripts/test_ssh_app_server_reconnect.py` on the SSH client host with
`--host <ssh-alias> --codex <remote-codex-path> --bridge <remote-built-bridge-path>`.
It creates a disposable backend and loopback mock model on the remote host through
an independent control connection. It then opens two real SSH client connections,
forcibly terminates them, and resumes the same saved thread through three further
SSH connections. Cleanup stops only the fixture's own backend and removes its
temporary home. Agent forwarding is disabled.

Both SSH checks passed from macOS to EVO Linux. This is actual SSH-client process
loss, not a claim of VS Code extension integration, TCP blackhole recovery, or
machine sleep/wake testing. The backend remains Linux in this test.

The `shared-app-server-transport` workflow runs the bridge's unit and subprocess
tests on Linux, macOS, and Windows. Those results cover transport behavior, not
end-to-end SDK/IDE lifecycle behavior on every platform. The Python shared-mode
unit fixtures now use platform-native absolute socket paths.

Still blocked or unsupported:

- VS Code launch/reconnect integration requires the extension source outside this repository.
- Permanent private owners require cooperative migration or a client/server ownership protocol;
  neither live-lock deletion nor forced owner termination is implemented.
- Native IDE sleep/wake, network blackholes, and full Windows SDK cancellation remain unvalidated.
  TypeScript shared mode explicitly rejects Windows rather than leaving abort behavior ambiguous.
- No upstream issue comment or PR has been posted.

Native bridge CI completed successfully on Linux, macOS, and Windows:
[run 39449222899](https://github.com/anotherempty/codex/actions/runs/39449222899).
Each platform ran all eight bridge unit/subprocess tests. The workflow tested
commit `5c7d3eeca6`; subsequent SSH-fixture lint and exec signal-error handling
changes do not modify the bridge.
