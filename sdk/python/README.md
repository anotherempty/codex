# OpenAI Codex Python SDK

Build Python applications that start Codex threads, run turns, stream progress,
and control workspace access.

## Install

Install the SDK:

```bash
pip install openai-codex
```

## Quickstart

The SDK reuses your existing Codex authentication when one is already
available:

```python
from openai_codex import Codex

with Codex() as codex:
    thread = codex.thread_start()
    result = thread.run("Explain this repository in three bullets.")
    print(result.final_response)
```

`thread.run(...)` returns a `TurnResult` containing the final response,
collected items, and token usage.

## Authentication

Existing Codex authentication is reused automatically. To start ChatGPT
browser login explicitly:

```python
from openai_codex import Codex

with Codex() as codex:
    login = codex.login_chatgpt()
    print(login.auth_url)
    print(login.wait().success)
```

For device-code login:

```python
with Codex() as codex:
    login = codex.login_chatgpt_device_code()
    print(login.verification_url, login.user_code)
    login.wait()
```

For API-key login:

```python
with Codex() as codex:
    codex.login_api_key("sk-...")
```

## Built-In Help

Use Python's standard `help(openai_codex)`, `help(Codex)`, or
`python -m pydoc openai_codex` documentation tools.

## Documentation

- [Getting started](https://github.com/openai/codex/blob/main/sdk/python/docs/getting-started.md)
- [API reference](https://github.com/openai/codex/blob/main/sdk/python/docs/api-reference.md)
- [Untrusted external messages](https://github.com/openai/codex/blob/main/sdk/python/docs/api-reference.md#externalmessage)
- [FAQ](https://github.com/openai/codex/blob/main/sdk/python/docs/faq.md)
- [Examples](https://github.com/openai/codex/blob/main/sdk/python/examples/README.md)

The package is licensed under the
[repository Apache License 2.0](https://github.com/openai/codex/blob/main/LICENSE).

### Connect to an existing shared app-server (experimental)

A CLI built from this branch can attach the SDK to an already running local
app-server using the JSON-RPC proxy:

```python
from openai_codex import Codex, CodexConfig

config = CodexConfig(
    codex_bin="/absolute/path/to/fork/codex",
    app_server_socket="/absolute/private/directory/server.sock",
)
with Codex(config=config) as codex:
    thread = codex.thread_resume("saved-thread-id")
```

The same configuration works with `AsyncCodex`. Provision the compatible server
separately (`codex app-server --listen unix:///absolute/private/directory/server.sock`).
This option does not start a server, fall back to a private server, or change its
configuration. It requires this branch's `app-server proxy --json-rpc` support;
the published pinned runtime does not provide that new flag.

The server owns credentials, environment, execution host, and configuration.
`env`, `config_overrides`, and `launch_args_override` are rejected with
`app_server_socket` rather than silently ignored. `cwd` sets the proxy process's
working directory; use thread parameters to select the thread working directory.
The socket path must be absolute.

Closing the SDK terminates only its proxy and detaches the connection. Active
turns continue on the server. Use explicit turn cancellation to interrupt work.
On connection loss, pending calls fail; create a **new** SDK instance, initialize
it (the public `Codex` API does this automatically), and resume the saved thread
ID. Inspect its current turn before submitting more work. Requests are never
automatically replayed, and existing iterators do not survive replacement of the
client. Do not use this mode when you require process death to cancel execution.
