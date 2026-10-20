#!/usr/bin/env python3
"""Manual integration regression: no credentials or external model requests.

Runs two servers in a temporary Codex home and connects through the newly built
bridge. Intended for Linux/macOS environments with Unix socket support.
"""

import argparse
import asyncio
import sys
import http.server
import json
import os
import pathlib
import queue
import signal
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description='Exercise shared app-server reconnect through the JSON-RPC bridge using a local mock model.')
parser.add_argument('--codex', required=True, help='Codex executable with app-server Unix socket support')
parser.add_argument('--bridge', required=True, help='Built codex-stdio-to-uds executable')
parser.add_argument('--python-sdk', action='store_true', help='Also exercise the checkout Python SDK (requires its dependencies)')
parser.add_argument('--exec', dest='exec_binary', help='Also exercise a built codex-exec shared client')
parser.add_argument('--typescript-sdk', action='store_true', help='Also exercise the built TypeScript SDK (requires --exec and Node on PATH)')
args = parser.parse_args()
if args.typescript_sdk and not args.exec_binary:
    parser.error('--typescript-sdk requires --exec')
B = str(pathlib.Path(args.codex).resolve())
bridge = str(pathlib.Path(args.bridge).resolve())
gate = threading.Event()
entered = threading.Event()
mock_events = queue.Queue()

class Mock(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        mock_event = mock_events.get_nowait() if not mock_events.empty() else None
        entered.set()
        gate.wait(60)
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [{'type': 'response.created', 'response': {'id': 'resp_test'}}, {'type': 'response.completed', 'response': {'id': 'resp_test', 'status': 'completed', 'output': [], 'usage': {'input_tokens': 1, 'output_tokens': 0, 'total_tokens': 1}}}]
        if mock_event is not None:
            events.insert(1, mock_event)
        for event in events:
            try:
                self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                break

class Client:

    def __init__(self, path):
        self.process = subprocess.Popen([bridge, '--json-rpc', path], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, env=env)
        self.messages = queue.Queue()
        self.n = 0
        self.pending_requests = []

        def read_output():
            try:
                for line in self.process.stdout:
                    self.messages.put(json.loads(line))
            finally:
                self.messages.put(None)
        self.reader = threading.Thread(target=read_output, daemon=True)
        self.reader.start()

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.n += 1
        self.send({'id': self.n, 'method': method, 'params': params})
        deadline = time.monotonic() + 25
        while True:
            message = self.messages.get(timeout=max(0, deadline - time.monotonic()))
            if message is None:
                raise RuntimeError('Bridge exited unexpectedly')
            if message.get('id') == self.n and ('result' in message or 'error' in message):
                return message
            if 'id' in message and 'method' in message:
                self.pending_requests.append(message)

    def init(self):
        result = self.rpc('initialize', {'clientInfo': {'name': 'reconnect_probe', 'version': '0.1'}, 'capabilities': {'experimentalApi': True}})
        assert 'result' in result, result
        self.send({'method': 'initialized'})

    def approval(self):
        for index, message in enumerate(self.pending_requests):
            if message.get("method") == "item/commandExecution/requestApproval":
                return self.pending_requests.pop(index)
        deadline = time.monotonic() + 15
        while True:
            message = self.messages.get(timeout=max(0, deadline - time.monotonic()))
            assert message is not None, "Bridge exited before approval"
            if message.get("method") == "item/commandExecution/requestApproval":
                return message

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdin.close()
        self.reader.join(timeout=5)
        self.process.stdout.close()
mock = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mock)
threading.Thread(target=mock.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='codex-reconnect-probe-') as tmp:
    env = dict(os.environ, CODEX_HOME=tmp)
    pathlib.Path(tmp, 'config.toml').write_text(f'model = "test-model"\nmodel_provider = "probe"\n[model_providers.probe]\nname = "Probe"\nbase_url = "http://127.0.0.1:{mock.server_port}/v1"\nwire_api = "responses"\n')
    log = open(tmp + '/server.log', 'w+')
    servers = []
    clients = []
    executions = []
    try:
        for name in ['shared', 'independent']:
            p = subprocess.Popen([B, 'app-server', '--listen', 'unix://' + tmp + '/' + name + '.sock'], env=env, stdout=log, stderr=log)
            servers.append(p)
            for _ in range(100):
                if pathlib.Path(tmp + '/' + name + '.sock').exists():
                    break
                time.sleep(0.1)

        def connect(name):
            c = Client(tmp + '/' + name + '.sock')
            clients.append(c)
            c.init()
            return c
        a = connect('shared')
        r = a.rpc('thread/start', {'cwd': tmp, 'model': 'test-model'})
        assert 'result' in r, r
        tid = r['result']['thread']['id']
        r = a.rpc('turn/start', {'threadId': tid, 'input': [{'type': 'text', 'text': 'test', 'text_elements': []}]})
        assert 'result' in r, r
        turn = r['result']['turn']['id']
        assert entered.wait(15), 'mock request never arrived'
        b = connect('shared')
        r = b.rpc('thread/resume', {'threadId': tid})
        assert 'result' in r, r
        print('PASS: shared server resumes active turn while original client remains connected', flush=True)
        other = connect('independent')
        r = other.rpc('thread/resume', {'threadId': tid})
        assert 'active writer' in r.get('error', {}).get('message', ''), r
        print('PASS: independent server reproduces active-writer conflict', flush=True)
        a.close()
        b.close()
        c = connect('shared')
        r = c.rpc('thread/resume', {'threadId': tid})
        assert 'result' in r, r
        assert any((t['id'] == turn and t['status'] == 'inProgress' for t in r['result']['thread']['turns'])), r
        print('PASS: reconnect preserves original active turn after abrupt disconnect', flush=True)
        for _ in range(5):
            c.close()
            c = connect('shared')
            r = c.rpc('thread/resume', {'threadId': tid})
            assert 'result' in r, r
        print('PASS: five successive bridge reconnects', flush=True)
        if args.python_sdk:
            sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "sdk/python/src"))
            from openai_codex.client import CodexClient, CodexConfig
            from openai_codex.async_client import AsyncCodexClient

            # Use the built standalone bridge through a CLI-shaped fixture. The
            # installed backend predates the fork's `proxy --json-rpc` flag.
            launcher = pathlib.Path(tmp, "proxy-cli")
            launcher.write_text(
                "#!" + sys.executable + "\nimport os, sys\n"
                "assert sys.argv[1:5] == ['app-server', 'proxy', '--json-rpc', '--sock']\n"
                + "os.execv(" + repr(bridge) + ", [" + repr(bridge)
                + ", '--json-rpc', sys.argv[5]])\n"
            )
            launcher.chmod(0o700)
            sdk_config = CodexConfig(codex_bin=str(launcher), app_server_socket=tmp + '/shared.sock')
            for _ in range(2):
                with CodexClient(sdk_config) as sdk:
                    sdk.initialize()
                    resumed = sdk.thread_resume(tid)
                    assert resumed.thread.id == tid
                    assert any(t.id == turn and t.status.value == 'inProgress' for t in resumed.thread.turns)
                assert servers[0].poll() is None
            print('PASS: Python SDK reconnects using fresh clients; close preserves active turn', flush=True)

            async def async_resume():
                sdk = AsyncCodexClient(sdk_config)
                await sdk.start()
                try:
                    await sdk.initialize()
                    resumed = await sdk.thread_resume(tid)
                    assert resumed.thread.id == tid
                    assert any(t.id == turn and t.status.value == 'inProgress' for t in resumed.thread.turns)
                finally:
                    await sdk.close()
            asyncio.run(async_resume())
            assert servers[0].poll() is None
            print('PASS: async Python SDK attaches and detaches without cancelling turn', flush=True)
        gate.set()
        for _ in range(50):
            r = c.rpc('thread/read', {'threadId': tid, 'includeTurns': True})
            if any((t['id'] == turn and t['status'] == 'completed' for t in r.get('result', {}).get('thread', {}).get('turns', []))):
                break
            time.sleep(0.1)
        else:
            raise AssertionError(r)
        print('PASS: original turn completes after reconnect', flush=True)
        if args.python_sdk:
            gate.clear()
            entered.clear()
            started = c.rpc('turn/start', {'threadId': tid, 'input': [{'type': 'text', 'text': 'cancel test', 'text_elements': []}]})
            assert 'result' in started, started
            cancel_turn = started['result']['turn']['id']
            assert entered.wait(15), 'cancellable mock request never arrived'
            with CodexClient(sdk_config) as sdk:
                sdk.initialize()
                sdk.thread_resume(tid)
                sdk.turn_interrupt(tid, cancel_turn)
                for _ in range(100):
                    state = sdk.thread_read(tid, include_turns=True)
                    if any(t.id == cancel_turn and t.status.value == 'interrupted' for t in state.thread.turns):
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError('explicit SDK cancellation did not interrupt the turn')
            assert servers[0].poll() is None
            gate.set()
            print('PASS: explicit Python SDK cancellation interrupts only its selected turn', flush=True)
        mock_events.put({"type": "response.output_item.done", "item": {
            "type": "function_call", "call_id": "approval-test", "name": "exec_command",
            "arguments": json.dumps({"cmd": "echo approval-test", "sandbox_permissions": "require_escalated", "justification": "Mock approval regression"})}})
        pending = c.rpc("thread/start", {"cwd": tmp, "model": "test-model", "approvalPolicy": "on-request", "sandbox": "read-only"})
        assert "result" in pending, pending
        approval_tid = pending["result"]["thread"]["id"]
        started = c.rpc("turn/start", {"threadId": approval_tid, "input": [{"type": "text", "text": "approval test", "text_elements": []}]})
        assert "result" in started, started
        original = c.approval()
        c.close()
        c = connect("shared")
        resumed = c.rpc("thread/resume", {"threadId": approval_tid})
        assert "result" in resumed, resumed
        replayed = c.approval()
        assert replayed["id"] == original["id"], (original, replayed)
        c.send({"id": replayed["id"], "result": {"decision": "decline"}})
        print("PASS: pending approval replayed after abrupt client disconnect", flush=True)
        if args.exec_binary:
            exec_binary = str(pathlib.Path(args.exec_binary).resolve())
            exec_env = env.copy()
            for key in ['CODEX_API_KEY', 'OPENAI_API_KEY', 'CODEX_EXEC_SERVER_URL', 'OPENAI_BASE_URL']:
                exec_env.pop(key, None)

            def execute(socket):
                process = subprocess.Popen([
                    exec_binary, '--app-server-socket', socket,
                    '--skip-git-repo-check', '--json', '--cd', tmp, 'exec shared test',
                ], env=exec_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                executions.append(process)
                return process

            normal = execute(tmp + '/shared.sock')
            output, errors = normal.communicate(timeout=30)
            assert normal.returncode == 0, (output, errors)
            events = [json.loads(line) for line in output.splitlines()]
            assert any(e.get('type') == 'turn.completed' for e in events), events
            assert servers[0].poll() is None
            print('PASS: shared exec completes without stopping daemon', flush=True)

            missing = execute(tmp + '/missing.sock')
            output, errors = missing.communicate(timeout=30)
            assert missing.returncode != 0, (output, errors)
            assert not any(json.loads(line).get('type') == 'thread.started' for line in output.splitlines())
            print('PASS: unavailable explicit socket fails without private fallback', flush=True)

            gate.clear()
            entered.clear()
            cancelled = execute(tmp + '/shared.sock')
            assert entered.wait(20), 'exec did not start model request'
            cancelled.send_signal(signal.SIGTERM)
            output, errors = cancelled.communicate(timeout=30)
            assert cancelled.returncode != 0, (output, errors)
            events = [json.loads(line) for line in output.splitlines()]
            exec_tid = next(e['thread_id'] for e in events if e.get('type') == 'thread.started')
            state = c.rpc('thread/read', {'threadId': exec_tid, 'includeTurns': True})
            assert state['result']['thread']['turns'][-1]['status'] == 'interrupted', state
            assert servers[0].poll() is None
            gate.set()
            print('PASS: shared exec SIGTERM interrupts its turn and preserves daemon', flush=True)

            gate.clear()
            entered.clear()
            disconnected = execute(tmp + '/independent.sock')
            assert entered.wait(20), 'exec did not reach independent backend'
            servers[1].kill()
            servers[1].wait(timeout=5)
            output, errors = disconnected.communicate(timeout=30)
            assert disconnected.returncode != 0, (output, errors)
            assert not any(json.loads(line).get('type') == 'turn.completed' for line in output.splitlines())
            gate.set()
            print('PASS: backend loss fails exec without reporting completion or replaying work', flush=True)

            if args.typescript_sdk:
                launcher = pathlib.Path(tmp, 'exec-cli')
                launcher.write_text(
                    '#!' + sys.executable + '\nimport os, sys\n'
                    "assert sys.argv[1] == 'exec'\n"
                    + 'os.execv(' + repr(exec_binary) + ', [' + repr(exec_binary) + ', *sys.argv[2:]])\n'
                )
                launcher.chmod(0o700)
                helper = pathlib.Path(__file__).with_name('shared_app_server_typescript_client.mjs')

                def typescript(mode):
                    process = subprocess.Popen([
                        'node', str(helper), str(launcher), tmp + '/shared.sock', tmp, mode,
                    ], env=exec_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    executions.append(process)
                    return process

                completed = typescript('complete')
                output, errors = completed.communicate(timeout=30)
                assert completed.returncode == 0, (output, errors)
                assert json.loads(output)['completed']
                assert servers[0].poll() is None
                print('PASS: public TypeScript SDK completes through shared exec', flush=True)

                gate.clear()
                entered.clear()
                aborted = typescript('abort')
                assert entered.wait(20), 'TypeScript SDK did not start model request'
                aborted.send_signal(signal.SIGUSR1)
                output, errors = aborted.communicate(timeout=30)
                assert aborted.returncode == 0, (output, errors)
                result = json.loads(output)
                assert result['aborted']
                for _ in range(100):
                    state = c.rpc('thread/read', {'threadId': result['threadId'], 'includeTurns': True})
                    if state['result']['thread']['turns'][-1]['status'] == 'interrupted':
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError(state)
                assert servers[0].poll() is None
                gate.set()
                print('PASS: TypeScript AbortSignal interrupts its shared turn and preserves daemon', flush=True)

        c.process.stdin.close()
        assert c.process.wait(timeout=5) == 0
        print('PASS: clean stdin EOF detaches bridge without stopping daemon', flush=True)
    except Exception:
        log.flush()
        log.seek(0)
        print(log.read()[-1500:])
        raise
    finally:
        gate.set()
        for execution in executions:
            if execution.poll() is None:
                execution.kill()
                execution.wait(timeout=5)
        for c in clients:
            c.close()
        for p in servers:
            p.terminate()
        for p in servers:
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()
        log.close()
        mock.shutdown()
