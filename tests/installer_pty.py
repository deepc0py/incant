#!/usr/bin/env python3
"""Run the installer with controlled terminal semantics for behavior tests."""

import os
import pty
import select
import signal
import sys
import time


SHELL_INTEGRATION_PROMPT = b"Install shell integration? [y/N] "
OLLAMA_PROMPTS = (
    b"Install Ollama now? [Y/n] ",
    b"Start Ollama now? [Y/n] ",
    b"Pull the default model (qwen2.5-coder:7b, ~4.7GB)? [Y/n] ",
)
TIMEOUT_SECONDS = 20


def run_prompt(answer: bytes, command: list[str]) -> int:
    ambiguous_master_fd, ambiguous_slave_fd = pty.openpty()
    pid, master_fd = pty.fork()
    if pid == 0:
        os.dup2(ambiguous_slave_fd, 0)
        os.close(ambiguous_master_fd)
        os.close(ambiguous_slave_fd)
        os.execvp(command[0], command)

    os.close(ambiguous_slave_fd)
    os.write(ambiguous_master_fd, b"n\n")

    output = bytearray()
    answer_sent = False
    answered_ollama_prompts: set[bytes] = set()
    child_status = None
    deadline = time.monotonic() + TIMEOUT_SECONDS

    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master_fd], [], [], 0.1)
            if readable:
                try:
                    chunk = os.read(master_fd, 4096)
                except OSError:
                    chunk = b""
                if chunk:
                    output.extend(chunk)
                    for ollama_prompt in OLLAMA_PROMPTS:
                        if (
                            ollama_prompt in output
                            and ollama_prompt not in answered_ollama_prompts
                        ):
                            os.write(master_fd, b"n")
                            answered_ollama_prompts.add(ollama_prompt)
                    if not answer_sent and SHELL_INTEGRATION_PROMPT in output:
                        os.write(master_fd, answer)
                        answer_sent = True

            waited_pid, status = os.waitpid(pid, os.WNOHANG)
            if waited_pid == pid:
                child_status = status
                break
        else:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            sys.stderr.write("installer PTY test timed out\n")
            return 124
    finally:
        os.close(ambiguous_master_fd)
        os.close(master_fd)
    sys.stdout.buffer.write(output)
    if not answer_sent:
        sys.stderr.write("installer never displayed the shell-integration prompt\n")
        return 125
    return os.waitstatus_to_exitcode(child_status)


def run_without_controlling_tty(command: list[str]) -> int:
    master_fd, slave_fd = pty.openpty()
    read_fd, write_fd = os.pipe()
    pid = os.fork()

    if pid == 0:
        try:
            os.setsid()
            worker_pid = os.fork()
            if worker_pid != 0:
                os.close(master_fd)
                os.close(slave_fd)
                os.close(read_fd)
                os.close(write_fd)
                _, worker_status = os.waitpid(worker_pid, 0)
                worker_exit = os.waitstatus_to_exitcode(worker_status)
                os._exit(worker_exit if worker_exit >= 0 else 128 - worker_exit)

            os.dup2(slave_fd, 0)
            os.dup2(write_fd, 1)
            os.dup2(write_fd, 2)
            os.close(master_fd)
            os.close(slave_fd)
            os.close(read_fd)
            os.close(write_fd)
            os.execvp(command[0], command)
        except OSError:
            os._exit(127)

    os.close(slave_fd)
    os.close(write_fd)
    output = bytearray()
    child_status = None
    deadline = time.monotonic() + TIMEOUT_SECONDS

    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([read_fd], [], [], 0.1)
            if readable:
                chunk = os.read(read_fd, 4096)
                if chunk:
                    output.extend(chunk)

            waited_pid, status = os.waitpid(pid, os.WNOHANG)
            if waited_pid == pid:
                child_status = status
                break
        else:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            sys.stdout.buffer.write(output)
            sys.stderr.write("installer no-controlling-TTY test timed out\n")
            return 124
    finally:
        os.close(read_fd)
        os.close(master_fd)

    sys.stdout.buffer.write(output)
    return os.waitstatus_to_exitcode(child_status)


def main() -> int:
    if len(sys.argv) < 3:
        sys.stderr.write(
            "usage: installer_pty.py prompt ANSWER COMMAND... | no-controlling-tty COMMAND...\n"
        )
        return 2

    mode = sys.argv[1]
    if mode == "prompt":
        if len(sys.argv) < 4:
            return 2
        return run_prompt(sys.argv[2].encode(), sys.argv[3:])
    if mode == "no-controlling-tty":
        return run_without_controlling_tty(sys.argv[2:])

    sys.stderr.write(f"unknown mode: {mode}\n")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
