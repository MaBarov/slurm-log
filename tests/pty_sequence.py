#!/usr/bin/env python3
"""Drive a command in a real PTY by waiting for prompts before sending input."""

from __future__ import annotations

import os
import pty
import select
import signal
import sys
import time


def decode_response(value: str) -> bytes:
    return value.replace(r"\r", "\r").replace(r"\n", "\n").encode()


def terminate(pid: int) -> None:
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    time.sleep(0.1)
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def main() -> int:
    if "--" not in sys.argv or len(sys.argv) < 6:
        raise SystemExit(
            "usage: pty_sequence.py TRANSCRIPT PROMPT RESPONSE ... -- COMMAND ..."
        )
    separator = sys.argv.index("--")
    transcript_path = sys.argv[1]
    plan = sys.argv[2:separator]
    command = sys.argv[separator + 1 :]
    if not command or len(plan) % 2:
        raise SystemExit("prompt/response pairs and a command are required")

    pid, master = pty.fork()
    if pid == 0:
        os.execvpe(command[0], command, os.environ)

    deadline = time.monotonic() + 30
    pending = bytearray()
    status: int | None = None
    try:
        with open(transcript_path, "wb") as transcript:
            for prompt, response in zip(plan[::2], plan[1::2]):
                needle = prompt.encode()
                while needle not in pending:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError(f"timed out waiting for prompt: {prompt}")
                    readable, _, _ = select.select([master], [], [], min(remaining, 0.1))
                    if not readable:
                        continue
                    chunk = os.read(master, 65536)
                    if not chunk:
                        raise RuntimeError(f"PTY closed before prompt: {prompt}")
                    transcript.write(chunk)
                    transcript.flush()
                    pending.extend(chunk)
                end = pending.index(needle) + len(needle)
                del pending[:end]
                # Some interactive programs print a label and establish their
                # next stdin mode in separate operations. Let that transition
                # finish so the response cannot be discarded with old input.
                time.sleep(0.05)
                os.write(master, decode_response(response))

            while status is None:
                waited, raw_status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    status = raw_status
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("timed out waiting for command to exit")
                readable, _, _ = select.select([master], [], [], min(remaining, 0.1))
                if readable:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        chunk = b""
                    if chunk:
                        transcript.write(chunk)
                        transcript.flush()
    except Exception as error:
        print(f"pty_sequence: {error}", file=sys.stderr)
        terminate(pid)
        os.waitpid(pid, 0)
        return 1
    finally:
        os.close(master)

    return os.waitstatus_to_exitcode(status)


if __name__ == "__main__":
    raise SystemExit(main())
