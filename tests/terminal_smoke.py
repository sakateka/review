#!/usr/bin/env python3
"""Exercise an installed review binary using a real POSIX PTY; no optional tools."""
import argparse
import codecs
import re
import unicodedata
import fcntl
import os
import pathlib
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=pathlib.Path)
parser.add_argument("--test-binary", type=pathlib.Path)
args = parser.parse_args()
binary = args.binary.resolve()
git = shutil.which("git")

with tempfile.TemporaryDirectory(prefix="review-pty-") as directory:
    root = pathlib.Path(directory)
    repo = root / "repo"
    repo.mkdir()
    def git_run(*words):
        return subprocess.check_output([git, *words], cwd=repo, stderr=subprocess.STDOUT)
    git_run("init", "-q")
    git_run("config", "user.name", "PTY Test")
    git_run("config", "user.email", "pty@example.invalid")
    (repo / "target.rs").write_text("fn main_target() {}\n")
    long_line = "\t// " + "界e\u0301🙂" * 150 + "\n"
    (repo / "caller.rs").write_text("fn caller() { main_target(); }\n" + long_line)
    fixture_root = pathlib.Path(__file__).resolve().parent / "fixtures" / "navigation"
    fixture_names = ["sample.rs", "sample.c", "sample.h", "sample.cpp", "sample.go", "sample.zig", "sample.py", "sample.sh", "sample.bash"]
    for name in fixture_names + ["sample.yaml", "sample.json"]:
        text = (fixture_root / name).read_text()
        extension = name.split(".")[-1]
        text = text.replace("target", "navtarget_" + extension + "_end").replace("port", "navport_" + extension)
        (repo / name).write_text(text)
    git_run("add", ".")
    git_run("commit", "-qm", "initial")
    (repo / "caller.rs").write_text("fn caller() { main_target(); main_target(); }\n" + long_line)
    tools = root / "tools"
    tools.mkdir()
    (tools / "git").symlink_to(git)
    env = dict(os.environ, TERM="xterm-256color", PATH=str(tools), HOME=str(root), XDG_STATE_HOME=str(root / "state"), GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
    before = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}

    class Session:
        def __init__(self, command=None, scenario=None):
            self.master, self.slave = pty.openpty()
            self.original = termios.tcgetattr(self.slave)
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 140, 0, 0))
            child_env = dict(env)
            if scenario:
                child_env["REVIEW_TERMINAL_SCENARIO"] = scenario
            self.process = subprocess.Popen(command or [str(binary), "--", "HEAD"], cwd=repo, env=child_env, stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True)
            self.output = bytearray()
            self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
            self.pending = ""
            self.cells = {}
            self.row = self.col = 0
            self.columns = 140
        def pump(self, seconds=0.2):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([self.master], [], [], max(0, end-time.monotonic()))[0]:
                    data = os.read(self.master, 65536)
                    self.output.extend(data)
                    self.feed(self.decoder.decode(data))
        def feed(self, text):
            self.pending += text
            while self.pending:
                if self.pending[0] == "\x1b":
                    match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                    if not match:
                        if len(self.pending) < 2 or self.pending.startswith("\x1b["):
                            return
                        self.pending = self.pending[2:]
                        continue
                    parameters, _, command = match.groups()
                    numbers = [int(n) if n else 0 for n in parameters.lstrip("?").split(";")]
                    value = numbers[0] if numbers else 0
                    if command in "Hf":
                        self.row = max(0, value-1)
                        self.col = max(0, (numbers[1] if len(numbers)>1 else 1)-1)
                    elif command == "A": self.row = max(0, self.row-(value or 1))
                    elif command == "B": self.row += value or 1
                    elif command == "C": self.col += value or 1
                    elif command == "D": self.col = max(0, self.col-(value or 1))
                    elif command == "G": self.col = max(0, value-1)
                    elif command == "J" and value in (2,3): self.cells.clear()
                    elif command == "K":
                        self.cells = {position:ch for position,ch in self.cells.items() if position[0] != self.row or position[1] < self.col}
                    elif command == "h" and parameters == "?1049": self.cells.clear()
                    self.pending = self.pending[match.end():]
                    continue
                ch, self.pending = self.pending[0], self.pending[1:]
                if ch == "\r": self.col = 0
                elif ch == "\n": self.row += 1
                elif ch >= " ":
                    width = 0 if unicodedata.combining(ch) else (2 if unicodedata.east_asian_width(ch) in "WF" else 1)
                    self.cells[(self.row,self.col)] = ch
                    self.col += width
        def text(self):
            return "\n".join("".join(self.cells.get((row,col)," ") for col in range(max(self.columns, 160))) for row in range(50))
        def wait_text(self, text, timeout=10):
            end = time.monotonic()+timeout
            while text not in self.text():
                self.pump(0.1)
                assert time.monotonic() < end, (text, self.text(), self.output[-500:])
        def send(self, data):
            os.write(self.master, data)
            self.pump()
        def resize(self, rows, cols):
            self.columns = cols
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            self.process.send_signal(signal.SIGWINCH)
            self.pump()
        def finish(self, expected=0):
            try:
                code = self.process.wait(timeout=10)
                self.pump()
                assert code == expected, (code, self.output[-5000:])
                assert termios.tcgetattr(self.slave) == self.original, "PTY termios not restored"
                assert b"\x1b[?1049l" in self.output, "alternate screen not restored"
                assert b"\x1b[?25h" in self.output, "cursor not restored"
            finally:
                if self.process.poll() is None:
                    self.process.kill()
                os.close(self.master)
                os.close(self.slave)

    session = Session()
    session.wait_text("Files")
    for name in fixture_names:
        symbol = "navtarget_" + name.split(".")[-1] + "_end"
        session.send(b"hs" + symbol.encode() + b"\r")
        session.wait_text("2 results")
        session.send(b"\r")
        session.wait_text("Captured old " + name)
        session.send(b"d")
        session.wait_text("Definitions candidates")
        session.wait_text("1 results")
        session.send(b"br")
        session.wait_text("References candidates")
        session.wait_text("1 results")
        session.send(b"h")
    for extension in ["yaml", "json"]:
        session.send(b"hsnavport_" + extension.encode() + b"\r")
        session.wait_text("Search candidates")
        session.send(b"\r")
        session.wait_text("Captured old sample." + extension)
        session.send(b"d")
        session.wait_text("Definitions candidates")
        session.send(b"br")
        session.wait_text("References candidates")
        session.wait_text("0 results")
        session.send(b"h")
    session.send(b"\r")
    session.wait_text("界")
    session.send(b"\x1b[C"*20 + b"\x1b[D"*5)
    session.send(b"hs")
    session.send(b"main_target\r")
    session.wait_text("results")
    session.send(b"\r")
    session.wait_text("Captured old")
    session.send(b"]r")
    session.wait_text("References")
    session.send(b"\rbbh")
    for shape in [(1, 1), (3, 8), (30, 140)]:
        session.resize(*shape)
    session.send(b" ")
    session.wait_text("Review mark saved")
    session.send(b"q")
    session.finish()
    for exit_method in ["q", "ctrl-c", "SIGINT", "SIGTERM"]:
        session = Session()
        session.wait_text("Files")
        session.wait_text("+ ")
        if exit_method.startswith("SIG"):
            session.process.send_signal(getattr(signal, exit_method))
        else:
            session.send(b"q" if exit_method == "q" else b"\x03")
        session.finish()
    session = Session([str(binary), "--", "missing-reference"])
    session.wait_text("Git status")
    session.send(b"q")
    session.finish(128)
    if args.test_binary:
        for scenario in ["panic", "error", "worker-panic"]:
            session = Session([str(args.test_binary.resolve()), "--exact", "tests::terminal_lifecycle_worker", "--nocapture"], scenario)
            if scenario == "worker-panic":
                session.wait_text("Files")
                session.send(b"s\r")
                session.wait_text("Analysis failed")
                assert termios.tcgetattr(session.slave) != session.original, "caught worker panic disabled raw mode"
                assert b"\x1b[?1049l" not in session.output, "caught worker panic left alternate screen"
                session.send(b"q")
            session.finish(101 if scenario == "panic" else 0)
    after = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    assert before == after, "review changed repository bytes"
print("PTY passed: installed navigation, side/back, marks/restart, tiny/resize, quit, Ctrl-C, SIGINT/SIGTERM, Git failure, repository byte preservation" + (", injected panic/I/O failure" if args.test_binary else ""))
