#!/usr/bin/env python3
"""Exercise an installed review binary using a real POSIX PTY; no optional tools."""
import argparse
import atexit
import codecs
import json
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
for option, expected in [("--help", "review ["), ("--version", "review ")]:
    result = subprocess.run([str(binary), option], capture_output=True, text=True, check=True)
    assert result.stdout.startswith(expected), result.stdout

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
    env.pop("NO_COLOR", None)  # The PTY models a color-capable terminal.
    before = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}

    class Session:
        def __init__(self, command=None, scenario=None, cwd=None):
            self.master, self.slave = pty.openpty()
            self.original = termios.tcgetattr(self.slave)
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 140, 0, 0))
            child_env = dict(env)
            if scenario:
                child_env["REVIEW_TERMINAL_SCENARIO"] = scenario
            self.process = subprocess.Popen(command or [str(binary), "--", "HEAD"], cwd=cwd or repo, env=child_env, stdin=self.slave, stdout=self.slave, stderr=self.slave, start_new_session=True)
            self.output = bytearray()
            self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
            self.pending = ""
            self.cells = {}
            self.colors = {}
            self.foreground = None
            self.row = self.col = 0
            self.columns = 140
            atexit.register(self.cleanup)
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
                    if command == "m":
                        i = 0
                        while i < len(numbers):
                            n = numbers[i]
                            if n in (0, 39): self.foreground = None
                            elif 30 <= n <= 37: self.foreground = n - 30
                            elif 90 <= n <= 97: self.foreground = n - 90 + 8
                            elif n in (38, 48) and i + 2 < len(numbers) and numbers[i+1] == 5:
                                if n == 38: self.foreground = numbers[i+2]
                                i += 2
                            elif n in (38, 48) and i + 4 < len(numbers) and numbers[i+1] == 2:
                                if n == 38: self.foreground = tuple(numbers[i+2:i+5])
                                i += 4
                            i += 1
                    elif command in "Hf":
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
                    self.colors[(self.row,self.col)] = self.foreground
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
                self.cleanup()
        def cleanup(self):
            atexit.unregister(self.cleanup)
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait()
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
    print("PTY passed: language navigation and terminal lifecycle", flush=True)

    # Story 6: run product acceptance through the installed executable, keeping
    # setup mutations distinct from read-only review sessions.
    acceptance = root / "acceptance"
    acceptance.mkdir()

    def setup_git(*words):
        return subprocess.check_output([git, *words], cwd=acceptance, env=env, stderr=subprocess.STDOUT)

    def snapshot(path):
        return {str(p.relative_to(path)): p.read_bytes() for p in path.rglob("*") if p.is_file()}

    setup_git("init", "-q")
    setup_git("config", "user.name", "Acceptance")
    setup_git("config", "user.email", "acceptance@example.invalid")
    for name in ["one.rs", "two.rs"]:
        (acceptance / name).write_text("fn " + name[:-3] + "() { let value = 1; }\n")
    setup_git("add", ".")
    setup_git("commit", "-qm", "base")
    left, right = "feat/base-work", "feat/next-work"
    setup_git("branch", left)
    for name in ["one.rs", "two.rs"]:
        (acceptance / name).write_text("fn " + name[:-3] + "() { let value = 2; }\n")
    setup_git("add", ".")
    setup_git("commit", "-qm", "both changed")
    setup_git("branch", right)

    def start(words, cwd=acceptance):
        return Session([str(binary), "--", *words], cwd=cwd)

    def closed(session):
        session.wait_text("Files")
        assert "┌Diff" not in session.text(), session.text()
        assert "let value" not in session.text(), session.text()

    def marks(session, expected):
        closed(session)
        for name, marked in expected.items():
            session.wait_text(("+ " if marked else "- ") + name)
            if marked:
                # Reviewed filenames must be dark gray (ANSI bright black),
                # including after restart, rather than merely showing a plus.
                for row, line in enumerate(session.text().splitlines()):
                    if ("+ " + name) in line:
                        col = line.index(name)
                        assert session.colors[(row, col)] == 8, (name, session.colors[(row, col)])
                        break

    def quit_unchanged(session, before, cwd=acceptance, expected=0):
        session.send(b"q")
        session.finish(expected)
        assert snapshot(cwd) == before, "installed review modified comparison inputs"

    # R2-R10: exact ticket invocation, explicit marks, closed previews and gray
    # names after restart, selective invalidation, ref movement and isolation.
    branch_args = [left, right]
    baseline = snapshot(acceptance)
    state_before = snapshot(root / "state")
    session = start(branch_args)
    marks(session, {"one.rs": False, "two.rs": False})
    session.send(b"j")
    closed(session)
    session.send(b"k\r")
    session.wait_text("let value = 2;")
    session.wait_text("Files")
    assert "+ one.rs" not in session.text(), "opening a diff implicitly marked it"
    assert snapshot(root / "state") == state_before, "viewing wrote review state"
    session.send(b"h ")
    session.wait_text("Review mark saved")
    session.send(b"j ")
    session.wait_text("+ two.rs")
    quit_unchanged(session, baseline)
    state_files = list((root / "state" / "review").glob("*.json"))
    assert any(len(json.loads(p.read_text())["marks"]) == 2 for p in state_files)

    session = start(branch_args)
    marks(session, {"one.rs": True, "two.rs": True})
    session.send(b"\r")
    session.wait_text("let value = 2;")
    quit_unchanged(session, baseline)
    session = start([left, right, "--", "one.rs"])
    marks(session, {"one.rs": False})
    assert "two.rs" not in session.text()
    quit_unchanged(session, baseline)

    (acceptance / "one.rs").write_text("fn one() { let value = 3; }\n")
    setup_git("add", ".")
    setup_git("commit", "-qm", "only one changed")
    setup_git("branch", "-f", right)
    baseline = snapshot(acceptance)
    session = start(branch_args)
    marks(session, {"one.rs": False, "two.rs": True})
    session.send(b" ")
    session.wait_text("Review mark saved")
    quit_unchanged(session, baseline)
    setup_git("commit", "--allow-empty", "-qm", "commit identity only")
    setup_git("branch", "-f", right)
    baseline = snapshot(acceptance)
    session = start(branch_args)
    marks(session, {"one.rs": True, "two.rs": True})
    quit_unchanged(session, baseline)

    # R1/R9/R12: native Git is the oracle for every short displayed patch or
    # original-output line; process status is compared independently.
    (acceptance / "one.rs").write_text("fn one() { let value = 4; }\n")
    setup_git("add", "one.rs")
    (acceptance / "two.rs").write_text("fn two() { let value = 5; }\n")
    cases = [
        (["--", "two.rs"], False),
        (["HEAD", "--", "two.rs"], False),
        (["--cached", "--", "one.rs"], False),
        ([left, right, "--", "one.rs"], False),
        ([left + ".." + right, "--", "one.rs"], False),
        ([left + "..." + right, "--", "one.rs"], False),
        (["--merge-base", left, right, "--", "one.rs"], False),
        ([left + ":one.rs", right + ":one.rs"], False),
        (["--exit-code", "HEAD", "--", "one.rs"], False),
        (["--stat", "HEAD"], True),
        (["--word-diff=porcelain", "HEAD", "--", "one.rs"], True),
        (["--output-indicator-new=>", "HEAD", "--", "one.rs"], True),
        (["--no-patch", "HEAD"], True),
        (["--quiet", "HEAD"], True),
    ]

    def check_native(words, original, cwd=acceptance):
        before = snapshot(cwd)
        oracle = subprocess.run([git, "diff", *words], cwd=cwd, env=env, capture_output=True)
        assert snapshot(cwd) == before, "oracle unexpectedly mutated fixture"
        session = start(words, cwd)
        session.wait_text("Original Git output" if original else "Files")
        if not original:
            session.send(b"\r")
        for line in oracle.stdout.decode().splitlines():
            if line.strip():
                session.wait_text(line.strip())
        if original:
            saved_before = snapshot(root / "state")
            session.send(b" ")
            session.wait_text("Marks are unavailable")
            session.send(b"s")
            session.wait_text("Navigation unavailable")
            assert "Search/outline:" not in session.text(), session.text()
            assert snapshot(root / "state") == saved_before, "opaque output wrote review marks"
        quit_unchanged(session, before, cwd, oracle.returncode)

    for words, original in cases:
        check_native(words, original)

    # Blob/no-index status and read-only filesystem behavior outside a repo.
    outside = root / "outside"
    outside.mkdir()
    (outside / "old.rs").write_text("fn outside() { let value = 1; }\n")
    (outside / "new.rs").write_text("fn outside() { let value = 2; }\n")
    no_index = ["--no-index", "old.rs", "new.rs"]
    check_native(no_index, False, outside)
    for iteration, changed in enumerate([False, False, True]):
        if changed:
            (outside / "new.rs").write_text("fn outside() { let value = 3; }\n")
        before = snapshot(outside)
        session = start(no_index, outside)
        closed(session)
        session.wait_text("- " if changed or iteration == 0 else "+ ")
        if changed or iteration == 0:
            session.send(b" ")
            session.wait_text("Review mark saved")
        quit_unchanged(session, before, outside, 1)

    # Staged/worktree mark invalidation uses the installed UI too.
    for words, name in [(["--cached"], "one.rs"), (["--", "two.rs"], "two.rs")]:
        before = snapshot(acceptance)
        session = start(words)
        session.wait_text("- " + name)
        session.send(b" ")
        session.wait_text("Review mark saved")
        quit_unchanged(session, before)
        session = start(words)
        marks(session, {name: True})
        quit_unchanged(session, before)
        (acceptance / name).write_text("fn changed_again() {}\n")
        if "--cached" in words:
            setup_git("add", name)
        before = snapshot(acceptance)
        session = start(words)
        marks(session, {name: False})
        quit_unchanged(session, before)

    # A genuine conflict produces combined output; preserve it without inventing
    # ordinary file marks or navigation, and retain Git's status.
    setup_git("reset", "--hard", "-q", "HEAD")
    setup_git("checkout", "-qb", "conflicting", left)
    (acceptance / "one.rs").write_text("fn conflict() {}\n")
    setup_git("add", ".")
    setup_git("commit", "-qm", "conflicting side")
    merge = subprocess.run([git, "merge", "--no-edit", right], cwd=acceptance, env=env, capture_output=True)
    assert merge.returncode == 1, merge.stdout
    check_native(["--cc"], True)

print("PTY passed: installed language navigation, terminal lifecycle, R1-R12 workflow, branch/ref/staged/worktree/no-index invalidation, Git compatibility and original-output fallback" + (", injected panic/I/O failure" if args.test_binary else ""))
