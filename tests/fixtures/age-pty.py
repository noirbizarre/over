"""Drive real terminal prompts without racing dialoguer's hidden-input setup."""

import errno
import json
import os
import pty
import select
import signal
import sys
import termios
import time

steps = json.loads(sys.argv[1])
split_stdout = os.environ.get("OVER_TEST_PTY_SPLIT_STDOUT") == "1"
if split_stdout:
    stdout_read, stdout_write = os.pipe()
pid, fd = pty.fork()
if pid == 0:
    if split_stdout:
        os.close(stdout_read)
        os.dup2(stdout_write, 1)
        os.close(stdout_write)
    os.execv(sys.argv[2], sys.argv[2:])
if split_stdout:
    os.close(stdout_write)

deadline = time.monotonic() + 20
output = b""
stdout = b""
pending = b""
sent = 0
while time.monotonic() < deadline:
    ready = select.select([fd, stdout_read] if split_stdout else [fd], [], [], 0.1)[0]
    if split_stdout and stdout_read in ready:
        stdout += os.read(stdout_read, 65536)
    if fd not in ready:
        continue
    try:
        block = os.read(fd, 65536)
    except OSError as error:
        if error.errno == errno.EIO:
            break
        raise
    if not block:
        break
    output += block
    pending += block
    if sent < len(steps) and steps[sent][0].encode() in pending:
        prompt, answer = steps[sent]
        if "passphrase" in prompt or "PIN" in prompt or "Secret value" in prompt:
            ready = time.monotonic() + 2
            while termios.tcgetattr(fd)[3] & termios.ECHO:
                if time.monotonic() >= ready:
                    raise RuntimeError("secret input did not disable echo")
                time.sleep(0.01)
        if answer == "\x03":
            os.killpg(pid, signal.SIGINT)
        else:
            os.write(fd, answer.encode())
        sent += 1
        pending = b""
else:
    os.killpg(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    sys.stderr.buffer.write(output) if split_stdout else sys.stdout.buffer.write(output)
    if split_stdout:
        sys.stdout.buffer.write(stdout)
    sys.exit(124)

_, status = os.waitpid(pid, 0)
if split_stdout:
    # Drain bytes sent just before terminal closure, preserving a separate binary stdout.
    while block := os.read(stdout_read, 65536):
        stdout += block
    os.close(stdout_read)
    sys.stdout.buffer.write(stdout)
    sys.stderr.buffer.write(output)
else:
    sys.stdout.buffer.write(output)
sys.exit(os.waitstatus_to_exitcode(status) if sent == len(steps) else 125)
