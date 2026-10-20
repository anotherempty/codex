from __future__ import annotations

from unittest.mock import Mock

import pytest

from openai_codex.client import CodexClient, CodexConfig


@pytest.mark.parametrize(
    "options, message",
    [
        ({"app_server_socket": "relative.sock"}, "absolute path"),
        ({"launch_args_override": ("custom",)}, "launch_args_override"),
        ({"config_overrides": ("model=other",)}, "configuration and environment"),
        ({"env": {"CODEX_API_KEY": "different"}}, "configuration and environment"),
        ({"env": {}}, "configuration and environment"),
    ],
)
def test_shared_backend_rejects_ignored_launch_options(monkeypatch, tmp_path, options, message):
    spawn = Mock()
    monkeypatch.setattr("openai_codex.client.subprocess.Popen", spawn)
    config = CodexConfig(**{"app_server_socket": str(tmp_path / "server.sock"), **options})
    with pytest.raises(ValueError, match=message):
        CodexClient(config).start()
    spawn.assert_not_called()


@pytest.mark.parametrize("shared", [False, True])
def test_backend_selection_and_proxy_only_shutdown(monkeypatch, tmp_path, shared):
    socket = str(tmp_path / "server.sock") if shared else None
    proc = Mock()
    spawn = Mock(return_value=proc)
    monkeypatch.setattr("openai_codex.client.subprocess.Popen", spawn)
    monkeypatch.setattr("openai_codex.client._resolve_codex_bin", lambda _: "/fork/codex")
    monkeypatch.setattr(CodexClient, "_start_reader_thread", lambda _: None)
    monkeypatch.setattr(CodexClient, "_start_stderr_drain_thread", lambda _: None)
    client = CodexClient(CodexConfig(codex_bin="/fork/codex", app_server_socket=socket))
    client.start()
    expected = ["proxy", "--json-rpc", "--sock", socket] if socket else ["--listen", "stdio://"]
    assert spawn.call_args.args[0] == ["/fork/codex", "app-server", *expected]
    assert (client._checkout_capabilities is None) == (socket is not None)
    client.close()
    proc.stdin.close.assert_called_once()
    proc.terminate.assert_called_once()
    proc.kill.assert_not_called()
    assert spawn.call_count == 1
