#!/usr/bin/env python3
"""Prove a packaged app-server reduces a large cell completed through wait."""

import argparse
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


TIMEOUT_SECONDS = 30
RAW_OUTPUT = "release-deferred-terminal-raw-output"
REPLACEMENT = "release selected deferred terminal replacement"
EXEC_CALL_ID = "release-deferred-exec"
WAIT_CALL_ID = "release-deferred-wait"


def sse(events: list[dict]) -> bytes:
    chunks = []
    for event in events:
        chunks.append(f"event: {event['type']}\n")
        chunks.append(f"data: {json.dumps(event, separators=(',', ':'))}\n\n")
    return "".join(chunks).encode()


def completed(response_id: str) -> dict:
    return {
        "type": "response.completed",
        "response": {
            "id": response_id,
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": None,
                "output_tokens": 0,
                "output_tokens_details": None,
                "total_tokens": 0,
            },
        },
    }


def output_text(output: object) -> str:
    if isinstance(output, str):
        return output
    if isinstance(output, list):
        return "".join(
            item.get("text", "") for item in output if isinstance(item, dict)
        )
    if isinstance(output, dict):
        return output_text(output.get("content", ""))
    return str(output)


class SmokeState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.model_requests: list[dict] = []
        self.reducer_requests: list[dict] = []
        self.acceptance_requests: list[dict] = []
        self.failure: str | None = None

    def fail(self, message: str) -> None:
        with self.lock:
            self.failure = self.failure or message


def make_handler(state: SmokeState):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, _format: str, *_args: object) -> None:
            return

        def send_json(self, status: int, body: dict) -> None:
            payload = json.dumps(body, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self) -> None:
            length = int(self.headers.get("content-length", "0"))
            try:
                body = json.loads(self.rfile.read(length))
            except Exception as error:
                state.fail(f"invalid JSON request for {self.path}: {error}")
                self.send_json(400, {})
                return

            if self.path == "/v1/reduce-code-mode-output":
                state.reducer_requests.append(body)
                if not (
                    body.get("script_status") == "Script completed"
                    and body.get("call_id") == EXEC_CALL_ID
                    and body.get("cell_id") == "1"
                    and RAW_OUTPUT in output_text(body.get("content_items"))
                ):
                    state.fail(f"invalid terminal reducer request: {body}")
                    self.send_json(200, {})
                    return
                self.send_json(
                    200,
                    {
                        "response_id": "release-deferred-gate",
                        "replacement": [{"type": "input_text", "text": REPLACEMENT}],
                    },
                )
                return

            if self.path == "/v1/accept-code-mode-output":
                state.acceptance_requests.append(body)
                self.send_response(204)
                self.end_headers()
                return

            if self.path != "/v1/responses":
                self.send_json(404, {})
                return

            request_index = len(state.model_requests)
            state.model_requests.append(body)
            if request_index == 0:
                source = (
                    '// @exec: {"yield_time_ms": 1, "max_output_tokens": 4096}\n'
                    'text("release-live-output\\n".repeat(200));\n'
                    "yield_control();\n"
                    "await new Promise(resolve => setTimeout(resolve, 100));\n"
                    f'text("{RAW_OUTPUT}\\n".repeat(2000));\n'
                )
                events = [
                    {"type": "response.created", "response": {"id": "resp-1"}},
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "type": "custom_tool_call",
                            "call_id": EXEC_CALL_ID,
                            "name": "exec",
                            "input": source,
                        },
                    },
                    completed("resp-1"),
                ]
            elif request_index == 1:
                events = [
                    {"type": "response.created", "response": {"id": "resp-2"}},
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "type": "function_call",
                            "call_id": WAIT_CALL_ID,
                            "name": "wait",
                            "arguments": json.dumps(
                                {"cell_id": "1", "yield_time_ms": 5000},
                                separators=(",", ":"),
                            ),
                        },
                    },
                    completed("resp-2"),
                ]
            elif request_index == 2:
                outputs = [
                    item
                    for item in body.get("input", [])
                    if item.get("type") == "function_call_output"
                    and item.get("call_id") == WAIT_CALL_ID
                ]
                if len(outputs) != 1:
                    state.fail(f"missing terminal wait output: {body}")
                else:
                    visible = output_text(outputs[0].get("output"))
                    if REPLACEMENT not in visible or RAW_OUTPUT in visible:
                        state.fail(f"terminal model output was not replaced: {visible}")
                events = [
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "type": "message",
                            "role": "assistant",
                            "id": "msg-done",
                            "content": [{"type": "output_text", "text": "done"}],
                        },
                    },
                    completed("resp-3"),
                ]
            else:
                state.fail(f"unexpected model request {request_index + 1}")
                events = [completed(f"resp-{request_index + 1}")]

            payload = sse(events)
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    return Handler


def send(process: subprocess.Popen[str], message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def wait_for(messages: queue.Queue[dict], predicate, label: str) -> dict:
    while True:
        try:
            message = messages.get(timeout=TIMEOUT_SECONDS)
        except queue.Empty as error:
            raise TimeoutError(f"timed out waiting for {label}") from error
        if predicate(message):
            return message


def run_smoke(app_server: Path) -> None:
    state = SmokeState()
    server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(state))
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    base_url = f"http://127.0.0.1:{server.server_port}"

    with tempfile.TemporaryDirectory() as temp_dir:
        home = Path(temp_dir)
        descriptor = home / "reducer.json"
        descriptor.write_text(
            json.dumps(
                {
                    "version": 1,
                    "url": f"{base_url}/v1/reduce-code-mode-output",
                    "acceptance_url": f"{base_url}/v1/accept-code-mode-output",
                    "token": "release-smoke-token",
                }
            )
        )
        (home / "config.toml").write_text(
            f'''model = "test-gpt-5.1-codex"
model_provider = "release_smoke"
approval_policy = "never"
sandbox_mode = "read-only"
suppress_unstable_features_warning = true

[model_providers.release_smoke]
name = "Release smoke"
base_url = "{base_url}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[features.code_mode]
enabled = true

[features.code_mode.output_reducer]
descriptor_path = "{descriptor}"
min_trigger_bytes = 100
timeout_ms = 5000
'''
        )
        environment = os.environ.copy()
        environment["CODEX_HOME"] = str(home)
        process = subprocess.Popen(
            [str(app_server)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        assert process.stdout is not None
        messages: queue.Queue[dict] = queue.Queue()

        def read_stdout() -> None:
            for line in process.stdout:
                messages.put(json.loads(line))

        threading.Thread(target=read_stdout, daemon=True).start()
        try:
            send(
                process,
                {
                    "id": 0,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {
                            "name": "pwragent-release-smoke",
                            "title": "PwrAgent release smoke",
                            "version": "1",
                        },
                        "capabilities": {"experimentalApi": True},
                    },
                },
            )
            wait_for(messages, lambda message: message.get("id") == 0, "initialize")
            send(process, {"method": "initialized"})
            send(process, {"id": 1, "method": "thread/start", "params": {}})
            started = wait_for(
                messages, lambda message: message.get("id") == 1, "thread/start"
            )
            thread_id = started["result"]["thread"]["id"]
            send(
                process,
                {
                    "id": 2,
                    "method": "turn/start",
                    "params": {
                        "threadId": thread_id,
                        "input": [{"type": "text", "text": "run deferred smoke"}],
                    },
                },
            )
            wait_for(messages, lambda message: message.get("id") == 2, "turn/start")
            wait_for(
                messages,
                lambda message: message.get("method") == "turn/completed",
                "turn/completed",
            )
            if state.failure:
                raise RuntimeError(state.failure)
            if len(state.model_requests) != 3:
                raise RuntimeError(
                    f"expected 3 model requests, got {len(state.model_requests)}"
                )
            if len(state.reducer_requests) != 1:
                raise RuntimeError(
                    f"expected one terminal reduction, got {len(state.reducer_requests)}"
                )
            if len(state.acceptance_requests) != 1:
                raise RuntimeError(
                    f"expected one acceptance, got {len(state.acceptance_requests)}"
                )
            acceptance = state.acceptance_requests[0]
            if not (
                acceptance.get("response_id") == "release-deferred-gate"
                and acceptance.get("call_id") == EXEC_CALL_ID
                and acceptance.get("cell_id") == "1"
            ):
                raise RuntimeError(f"invalid acceptance: {acceptance}")
        except Exception as error:
            process.kill()
            process.wait()
            assert process.stderr is not None
            stderr = process.stderr.read()
            raise RuntimeError(f"{error}\napp-server stderr:\n{stderr}") from error
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    server.shutdown()
    server.server_close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("app_server", type=Path)
    args = parser.parse_args()
    if not args.app_server.is_file():
        parser.error(f"app-server does not exist: {args.app_server}")
    try:
        run_smoke(args.app_server)
    except Exception as error:
        print(f"deferred Code Mode reducer smoke failed: {error}", file=sys.stderr)
        return 1
    print(f"deferred Code Mode reducer smoke passed: {args.app_server}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
