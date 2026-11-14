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
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[2], sys.argv[2:])

deadline = time.monotonic() + 20
output = b""
pending = b""
sent = 0
while time.monotonic() < deadline:
    if not select.select([fd], [], [], 0.1)[0]:
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
        if "passphrase" in prompt or "PIN" in prompt:
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
    sys.stdout.buffer.write(output)
    sys.exit(124)

_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(output)
sys.exit(os.waitstatus_to_exitcode(status) if sent == len(steps) else 125)
