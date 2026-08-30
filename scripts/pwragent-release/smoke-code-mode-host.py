#!/usr/bin/env python3
"""Exercise one JavaScript cell through a packaged code-mode host."""

import argparse
import json
import os
import selectors
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import BinaryIO


TIMEOUT_SECONDS = 15
SMOKE_TEXT = "pwragent-code-mode-smoke"


def write_frame(stream: BinaryIO, message: object) -> None:
    payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
    stream.write(struct.pack("<I", len(payload)))
    stream.write(payload)
    stream.flush()


def read_exact(stream: BinaryIO, size: int, deadline: float) -> bytes:
    selector = selectors.DefaultSelector()
    selector.register(stream, selectors.EVENT_READ)
    data = bytearray()
    try:
        while len(data) < size:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise TimeoutError("timed out waiting for code-mode host output")
            chunk = os.read(stream.fileno(), size - len(data))
            if not chunk:
                raise EOFError("code-mode host closed its stdout")
            data.extend(chunk)
    finally:
        selector.close()
    return bytes(data)


def read_frame(stream: BinaryIO, deadline: float) -> object:
    frame_size = struct.unpack("<I", read_exact(stream, 4, deadline))[0]
    return json.loads(read_exact(stream, frame_size, deadline))


def require_equal(actual: object, expected: object, label: str) -> None:
    if actual != expected:
        raise RuntimeError(
            f"unexpected {label}:\n"
            f"expected={json.dumps(expected, sort_keys=True)}\n"
            f"actual={json.dumps(actual, sort_keys=True)}"
        )


def run_smoke(host_path: Path) -> None:
    process = subprocess.Popen(
        [str(host_path), "--listen", "stdio://"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdin is not None
    assert process.stdout is not None
    assert process.stderr is not None
    deadline = time.monotonic() + TIMEOUT_SECONDS
    try:
        write_frame(
            process.stdin,
            {
                "type": "connection/hello",
                "supportedVersions": [1],
                "requiredCapabilities": [],
                "optionalCapabilities": [],
            },
        )
        require_equal(
            read_frame(process.stdout, deadline),
            {
                "type": "connection/ready",
                "selectedVersion": 1,
                "capabilities": [],
            },
            "handshake response",
        )

        write_frame(
            process.stdin,
            {
                "type": "operation/request",
                "id": 1,
                "request": {
                    "method": "session/open",
                    "sessionId": "release-smoke",
                },
            },
        )
        require_equal(
            read_frame(process.stdout, deadline),
            {
                "type": "operation/response",
                "id": 1,
                "result": {
                    "status": "ok",
                    "value": {
                        "type": "session/ready",
                        "sessionId": "release-smoke",
                    },
                },
            },
            "session-open response",
        )

        write_frame(
            process.stdin,
            {
                "type": "operation/request",
                "id": 2,
                "request": {
                    "method": "session/execute",
                    "sessionId": "release-smoke",
                    "request": {
                        "tool_call_id": "release-smoke-call",
                        "enabled_tools": [],
                        "source": f'text("{SMOKE_TEXT}");',
                        "yield_time_ms": 10_000,
                        "max_output_tokens": 100,
                    },
                },
            },
        )
        require_equal(
            read_frame(process.stdout, deadline),
            {
                "type": "operation/response",
                "id": 2,
                "result": {
                    "status": "ok",
                    "value": {
                        "type": "execution/started",
                        "cellId": "1",
                    },
                },
            },
            "execute-start response",
        )
        require_equal(
            read_frame(process.stdout, deadline),
            {
                "type": "execute/initialResponse",
                "id": 2,
                "result": {
                    "status": "ok",
                    "value": {
                        "Result": {
                            "cell_id": "1",
                            "content_items": [
                                {"type": "input_text", "text": SMOKE_TEXT}
                            ],
                            "error_text": None,
                        }
                    },
                },
            },
            "execute-result response",
        )
    except Exception as error:
        process.kill()
        process.wait()
        stderr = process.stderr.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{error}\ncode-mode host stderr:\n{stderr}") from error
    finally:
        if process.poll() is None:
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("host", type=Path)
    args = parser.parse_args()
    if not args.host.is_file():
        parser.error(f"code-mode host does not exist: {args.host}")
    try:
        run_smoke(args.host)
    except Exception as error:
        print(f"code-mode host smoke test failed: {error}", file=sys.stderr)
        return 1
    print(f"code-mode host smoke test passed: {args.host}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
