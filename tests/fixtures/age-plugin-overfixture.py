#!/usr/bin/env python3
"""Protocol fixture only: file keys are intentionally NOT cryptographically wrapped."""

import base64
import os
import sys


def receive():
    header = sys.stdin.buffer.readline().decode().strip().split()
    assert header[0] == "->", header
    body = b""
    while True:
        line = sys.stdin.buffer.readline().strip()
        body += line
        if len(line) < 64:
            break
    return header[1:], base64.b64decode(body + b"=" * (-len(body) % 4))


def send(header, body=b"", acknowledge=True):
    # Record protocol command names only, never PINs, identities, or file-key bodies.
    with open(os.environ["OVER_TEST_PLUGIN_LOG"], "a") as log:
        log.write(header.split()[0] + "\n")
    encoded = base64.b64encode(body).rstrip(b"=")
    sys.stdout.buffer.write(("-> " + header + "\n").encode())
    for start in range(0, len(encoded), 64):
        sys.stdout.buffer.write(encoded[start:start + 64] + b"\n")
    if len(encoded) % 64 == 0:
        sys.stdout.buffer.write(b"\n")
    sys.stdout.buffer.flush()
    return receive() if acknowledge else None


mode = os.environ.get("OVER_TEST_PLUGIN_MODE", "success")
with open(os.environ["OVER_TEST_PLUGIN_LOG"], "a") as log:
    log.write(sys.argv[1] + "\n")

commands = []
while True:
    header, body = receive()
    if header[0] == "done":
        break
    commands.append((header, body))

if mode == "malformed":
    sys.stdout.write("this is not an age stanza\n")
    sys.exit(0)

decrypting = sys.argv[1] == "--age-plugin=identity-v1"
if mode == "absent":
    # The client must not reflect potentially secret-bearing plugin error bodies.
    send("error identity 0", b"device unavailable: private-error-sentinel")
elif decrypting:
    stanzas = [body for header, body in commands
               if header[:3] == ["recipient-stanza", "0", "overfixture"]]
    if stanzas and mode == "pin":
        response, pin = send("request-secret", b"Device PIN")
        if response != ["ok"] or pin != b"pin-sentinel":
            send("error identity 0", b"incorrect PIN: private-error-sentinel")
            stanzas = []
    if stanzas and mode == "cancel":
        response, _ = send("confirm eWVz bm8", b"Allow decryption?")
        if response != ["ok", "yes"]:
            send("error identity 0", b"confirmation denied: private-error-sentinel")
            stanzas = []
    if stanzas and mode == "interactive":
        send("msg", b"Insert device and touch it")
        accepted, _ = send("confirm eWVz bm8", b"Allow decryption?")
        public, _ = send("request-public", b"Device selector")
        secret, pin = send("request-secret", b"Device PIN")
        if accepted != ["ok", "yes"] or public != ["ok"] or secret != ["ok"] or pin != b"pin-sentinel":
            send("error identity 0", b"interaction denied: private-error-sentinel")
            stanzas = []
    if stanzas:
        send("file-key 0", stanzas[0])
else:
    key = next(body for header, body in commands if header[0] == "wrap-file-key")
    send("recipient-stanza 0 overfixture", key)

send("done", acknowledge=False)
