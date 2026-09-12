#!/usr/bin/env python3
"""Record an *interactive* command on a real pty into asciicast v2.

`script` needs a controlling terminal and there is none here; `pty` is stdlib.
The child sees `isatty() == True`, so the CLI takes its terminal path — the
session header, the prompt, the live turn — rather than the plain stream it
writes when piped.

Keystrokes are scheduled rather than typed, so the recording is reproducible.
Everything on screen, including the echo of what is typed, comes back from the
program through the pty; the timings are the real ones.

  ptyrec.py <cols> <rows> <out.cast> <script.json> -- <argv...>

script.json is [[delay_seconds, "text to send"], ...].
"""
import codecs
import json
import os
import pty
import select
import signal
import struct
import sys
import termios
import fcntl
import time

cols, rows, out_path, script_path = (
    int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4])
argv = sys.argv[sys.argv.index("--") + 1:]
sends = json.load(open(script_path))

pid, fd = pty.fork()
if pid == 0:
    env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(cols),
               LINES=str(rows), FORCE_COLOR="3")
    os.execvpe(argv[0], argv, env)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

events, start = [], time.time()
last_out = start

# One decoder across every read, not one per chunk.
#
# A pty hands back whatever happened to be in the buffer, so a multi-byte
# character lands split across two `os.read` calls whenever the boundary falls
# inside it. Decoding each chunk on its own turns both halves into U+FFFD — and
# when the split lands inside an escape sequence instead, the emulator misparses
# the repaint and the TUI's footer is drawn twice. Both artifacts in the first
# cast were this, not the renderer. An incremental decoder holds the partial
# sequence until the rest arrives.
decoder = codecs.getincrementaldecoder("utf-8")("replace")
queue = list(sends)
next_at = start + (queue[0][0] if queue else 1e9)

# The tail is `TRAILING_QUIET` after the last byte, not after the last
# keystroke: the recording should end on the finished screen. Letting it run on
# to a `/exit` catches the TUI tearing itself down, and the last frame of the
# loop is then a half-repainted status bar.
TRAILING_QUIET = 1.2

while True:
    timeout = max(0.0, next_at - time.time()) if queue else TRAILING_QUIET
    try:
        r, _, _ = select.select([fd], [], [], min(timeout, 5.0))
    except InterruptedError:
        continue

    if r:
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        text = decoder.decode(data)
        # An empty result means the chunk ended mid-character; the bytes are
        # held and arrive with the next read.
        if text:
            last_out = time.time()
            events.append([round(last_out - start, 4), "o", text])

    if queue and time.time() >= next_at:
        _, text = queue.pop(0)
        os.write(fd, text.encode())
        if queue:
            next_at = time.time() + queue[0][0]

    if not queue and time.time() - last_out > TRAILING_QUIET:
        break

# The child is still sitting at its prompt, so it has to be told to stop —
# `waitpid` on a live interactive shell never returns.
os.close(fd)
try:
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
except (ProcessLookupError, ChildProcessError):
    pass

# Hold the finished screen before the loop restarts. A cast ends at its last
# *byte*, and the answer arrives milliseconds after the question is sent — so
# without this the recording is six seconds of typing and a flash of the reply,
# which as a loop is unreadable. `ESC[0m` is a reset: a real byte that draws
# nothing, which is all this needs to extend the timeline.
LINGER = 3.5
if events:
    events.append([round(events[-1][0] + LINGER, 4), "o", "\x1b[0m"])

header = {"version": 2, "width": cols, "height": rows, "timestamp": 0,
          "env": {"TERM": "xterm-256color", "SHELL": "/bin/zsh"},
          "title": "ghostai chat"}
with open(out_path, "w") as f:
    f.write(json.dumps(header) + "\n")
    for e in events:
        f.write(json.dumps(e) + "\n")

print(f"{len(events)} events, {events[-1][0] if events else 0:.1f}s")
