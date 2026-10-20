#!/usr/bin/env python3
"""Exercise real SSH-client loss against a disposable remote shared app-server.

Run on the SSH client host. Uses a credential-free loopback mock model. Active-turn
and approval coverage lives in test_shared_app_server_reconnect.py. Requires Unix remote host.
"""

import argparse
import json
import queue
import shlex
import subprocess
import threading
import time

REMOTE_SERVER = r"""
import http.server, json, os, pathlib, subprocess, sys, tempfile, threading, time
class Mock(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for event in [
            {'type': 'response.created', 'response': {'id': 'ssh-probe'}},
            {'type': 'response.completed', 'response': {'id': 'ssh-probe', 'status': 'completed', 'output': [], 'usage': {'input_tokens': 1, 'output_tokens': 0, 'total_tokens': 1}}},
        ]:
            self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
            self.wfile.flush()
mock = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mock)
threading.Thread(target=mock.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix="codex-ssh-reconnect-") as directory:
    socket = str(pathlib.Path(directory, "server.sock"))
    pathlib.Path(directory, "config.toml").write_text(
        'model="test-model"\nmodel_provider="probe"\n[model_providers.probe]\n'
        f'name="Probe"\nbase_url="http://127.0.0.1:{mock.server_port}/v1"\nwire_api="responses"\n'
    )
    env = dict(os.environ, CODEX_HOME=directory)
    for key in ["CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_EXEC_SERVER_URL"]:
        env.pop(key, None)
    with open(pathlib.Path(directory, "server.log"), "w+") as log:
        server = subprocess.Popen([sys.argv[1], "app-server", "--listen", "unix://" + socket],
                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=env)
        try:
            for _ in range(200):
                if pathlib.Path(socket).exists():
                    break
                if server.poll() is not None:
                    raise RuntimeError("disposable backend exited during startup")
                time.sleep(0.05)
            else:
                raise RuntimeError("disposable backend did not create its socket")
            print(json.dumps({"socket": socket, "cwd": directory}), flush=True)
            sys.stdin.readline()  # Parent keeps this control connection separate from clients.
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
            mock.shutdown()
"""


def messages(process):
    incoming = queue.Queue()

    def read():
        try:
            for line in process.stdout:
                incoming.put(json.loads(line))
        except (OSError, ValueError) as error:
            incoming.put(error)
        finally:
            incoming.put(None)

    threading.Thread(target=read, daemon=True).start()
    return incoming


def receive(incoming):
    value = incoming.get(timeout=30)
    if isinstance(value, BaseException):
        raise value
    if value is None:
        raise RuntimeError("SSH connection closed unexpectedly")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--host", required=True, help="Existing SSH alias (no agent forwarding)"
    )
    parser.add_argument(
        "--codex", required=True, help="Absolute remote Codex executable"
    )
    parser.add_argument(
        "--bridge", required=True, help="Absolute remote built JSON-RPC bridge"
    )
    args = parser.parse_args()
    ssh = [
        "ssh",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ForwardAgent=no",
        args.host,
    ]
    control = subprocess.Popen(
        [*ssh, shlex.join(["python3", "-u", "-c", REMOTE_SERVER, args.codex])],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    clients = []
    counter = 0

    def connect(socket):
        process = subprocess.Popen(
            [*ssh, shlex.join([args.bridge, "--json-rpc", socket])],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        clients.append(process)
        incoming = messages(process)
        rpc(
            process,
            incoming,
            "initialize",
            {
                "clientInfo": {"name": "ssh_reconnect_probe", "version": "0.1"},
                "capabilities": {"experimentalApi": True},
            },
        )
        process.stdin.write(json.dumps({"method": "initialized"}) + "\n")
        process.stdin.flush()
        return process, incoming

    def rpc(process, incoming, method, params):
        nonlocal counter
        counter += 1
        process.stdin.write(
            json.dumps({"id": counter, "method": method, "params": params}) + "\n"
        )
        process.stdin.flush()
        while True:
            message = receive(incoming)
            if message.get("id") == counter:
                assert "result" in message, message
                return message["result"]

    try:
        backend = receive(messages(control))
        first, incoming = connect(backend["socket"])
        thread_id = rpc(
            first,
            incoming,
            "thread/start",
            {"cwd": backend["cwd"], "model": "test-model"},
        )["thread"]["id"]
        turn = rpc(
            first,
            incoming,
            "turn/start",
            {
                "threadId": thread_id,
                "input": [
                    {
                        "type": "text",
                        "text": "SSH persistence probe",
                        "text_elements": [],
                    }
                ],
            },
        )["turn"]["id"]
        for _ in range(100):
            state = rpc(
                first,
                incoming,
                "thread/read",
                {"threadId": thread_id, "includeTurns": True},
            )
            if any(
                item["id"] == turn and item["status"] == "completed"
                for item in state["thread"]["turns"]
            ):
                break
            time.sleep(0.1)
        else:
            raise AssertionError("mock turn did not complete")
        second, second_incoming = connect(backend["socket"])
        assert (
            rpc(second, second_incoming, "thread/resume", {"threadId": thread_id})[
                "thread"
            ]["id"]
            == thread_id
        )
        print(
            "PASS: two independent SSH clients resume the same shared thread",
            flush=True,
        )
        first.kill()
        first.wait(timeout=10)
        second.kill()
        second.wait(timeout=10)
        for _ in range(3):
            current, incoming = connect(backend["socket"])
            assert (
                rpc(current, incoming, "thread/resume", {"threadId": thread_id})[
                    "thread"
                ]["id"]
                == thread_id
            )
            current.kill()
            current.wait(timeout=10)
        print(
            "PASS: three reconnects after forcibly terminating real SSH clients",
            flush=True,
        )
        assert control.poll() is None
    finally:
        for client in clients:
            if client.poll() is None:
                client.kill()
            client.wait(timeout=10)
            client.stdin.close()
            client.stdout.close()
        control.stdin.close()
        try:
            result = control.wait(timeout=20)
        except subprocess.TimeoutExpired:
            control.kill()
            control.wait()
            raise
        control.stdout.close()
        if result != 0:
            raise RuntimeError(f"remote fixture cleanup exited with {result}")


if __name__ == "__main__":
    main()
