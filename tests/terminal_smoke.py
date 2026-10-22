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
    (repo / "docs.rs").write_text("/// Captured ambiguous documentation.\nfn doc_choice() {}\nmod other {\nfn doc_choice() {}\n}\nfn docs_caller() { doc_choice(); }\n")
    (repo / "target.rs").write_text("/// Captured terminal documentation.\nfn main_target() {}\n")
    preview_caller = "fn preview_caller() { preview_unique(); let _: PreviewType; }"
    preview_ambiguous_caller = "fn preview_ambiguous_caller() { preview_ambiguous(); }"
    (repo / "preview.rs").write_text(
        "struct PreviewType { value: usize }\nfn preview_unique() {}\n"
        + preview_caller + "\nmod one { fn preview_ambiguous() {} }\n"
        "mod two { fn preview_ambiguous() {} }\n" + preview_ambiguous_caller + "\n"
        "fn variable_owner() { let preview_variable = 1; }\n"
    )
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
    env = dict(os.environ, TERM="xterm-256color", PATH=str(tools), HOME=str(root), XDG_STATE_HOME=str(root / "state"), XDG_CONFIG_HOME=str(root / "config"), GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
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
                for mode in (1000, 1002, 1003, 1006, 1015):
                    assert f"\x1b[?{mode}h".encode() in self.output, ("mouse capture not enabled", mode)
                    assert f"\x1b[?{mode}l".encode() in self.output, ("mouse capture not restored", mode)
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
        session.send(b"/" + symbol.encode() + b"\r")
        session.wait_text("2 results")
        session.send(b"\r")
        session.wait_text("Captured old " + name)
        session.send(b"gd")
        session.wait_text("Definitions candidates")
        session.wait_text("1 results")
        session.send(b"\x0fgr")
        session.wait_text("References candidates")
        session.wait_text("1 results")
        session.send(b"\x1b")
    for extension in ["yaml", "json"]:
        session.send(b"/navport_" + extension.encode() + b"\r")
        session.wait_text("Search candidates")
        session.send(b"\r")
        session.wait_text("Captured old sample." + extension)
        session.send(b"gd")
        session.wait_text("Definitions candidates")
        session.send(b"\x0fgr")
        session.wait_text("References candidates")
        session.wait_text("0 results")
        session.send(b"\x1b")
    session.send(b"\r")
    session.wait_text("界")
    session.send(b"\x1b[C"*20 + b"\x1b[D"*5)
    session.send(b"\x1b")
    session.send(b"/")
    session.send(b"main_target\r")
    session.wait_text("results")
    session.send(b"\r")
    session.wait_text("Captured old")
    session.send(b"]gr")
    session.wait_text("References")
    session.send(b"\r\x0f\x0f")
    session.send(b"\x1b")
    for shape in [(1, 1), (3, 8), (30, 140)]:
        session.resize(*shape)
    session.send(b" ")
    session.wait_text("Review mark saved")
    session.send(b",q")
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
    session.send(b",q")
    session.finish(128)
    if args.test_binary:
        for scenario in ["panic", "error", "worker-panic"]:
            session = Session([str(args.test_binary.resolve()), "--exact", "tests::terminal_lifecycle_worker", "--nocapture"], scenario)
            if scenario == "worker-panic":
                session.wait_text("Files")
                session.send(b"/\r")
                session.wait_text("Analysis failed")
                assert termios.tcgetattr(session.slave) != session.original, "caught worker panic disabled raw mode"
                assert b"\x1b[?1049l" not in session.output, "caught worker panic left alternate screen"
                session.send(b",q")
            session.finish(101 if scenario == "panic" else 0)
    after = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    assert before == after, "review changed repository bytes"
    print("PTY passed: language navigation and terminal lifecycle", flush=True)

    # Config and keymap acceptance uses the installed executable and actual
    # terminal modifier events, independent of the in-process dispatch tests.
    config_before = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    config_dir = root / "config" / "review"
    config_dir.mkdir(parents=True)
    config_file = config_dir / "config.toml"
    config_file.write_text('[features]\nkeymap = "emacs"\nreadline = false\n')
    session = Session()
    session.wait_text("Files")
    session.send(b"hl")
    assert "┌Diff" not in session.text(), "legacy file-pane h/l opened a diff"
    session.send(b"\r")
    session.wait_text("Diff")
    origin = (session.row, session.col)
    session.send(b"jdrs")
    assert (session.row, session.col) == origin, "legacy/Vim keys leaked into Emacs map"
    assert "candidates for" not in session.text()
    session.send(b"\x0e")  # C-n
    assert (session.row, session.col) == (origin[0] + 1, origin[1])
    session.send(b"\x10")  # C-p
    assert (session.row, session.col) == origin
    session.send(b"\x06")  # C-f
    assert (session.row, session.col) == (origin[0], origin[1] + 1)
    session.send(b"\x02")  # C-b
    assert (session.row, session.col) == origin
    session.send(b"\x05")  # C-e
    assert session.col > origin[1]
    session.send(b"\x01")  # C-a
    assert (session.row, session.col) == origin
    session.send(b"/main_target\r")
    session.wait_text("Search candidates")
    session.send(b"\r")
    session.wait_text("Captured old target.rs")
    session.send(b"\x1b.")  # M-.
    session.wait_text("Definitions candidates")
    session.send(b"\x1b,")  # M-,
    session.wait_text("Captured old target.rs")
    session.send(b"\x1b")
    assert session.process.poll() is None, "Emacs Escape exited"
    session.send(b",q")
    session.finish()

    # CLI style wins discovery, with readline disabled independently of Vim.
    session = Session([str(binary), "--keymap", "vim", "--", "HEAD"])
    session.wait_text("Files")
    session.send(b"\r")
    session.wait_text("Diff")
    origin = (session.row, session.col)
    session.send(b"\x0e")
    assert (session.row, session.col) == origin, "disabled readline moved the cursor"
    session.send(b"j")
    assert session.row == origin[0] + 1, "CLI keymap did not override discovered Emacs map"
    session.send(b",q")
    session.finish()

    # Supplemental readline works in Vim when its action is not overridden.
    readline_config = root / "readline.toml"
    readline_config.write_text('[features]\nkeymap="vim"\nreadline=true\n')
    session = Session([str(binary), "--config", str(readline_config), "--", "HEAD"])
    session.wait_text("Files")
    session.send(b"\r")
    session.wait_text("Diff")
    origin = (session.row, session.col)
    session.send(b"\x0e")
    assert session.row == origin[0] + 1, "enabled readline failed in Vim diff"
    session.send(b",q")
    session.finish()

    # Overrides replace defaults, support sequences and update the legend.
    explicit_config = root / "custom.toml"
    explicit_config.write_text('''[features]
keymap = "vim"
readline = true
[keybindings]
close_diff_view = "x"
cursor_down = "z"
go_to_definition = "gD"
''')
    session = Session([str(binary), "--config", str(explicit_config), "--", "HEAD"])
    session.wait_text("Files")
    session.send(b"\r")
    session.wait_text("Diff")
    origin = (session.row, session.col)
    session.send(b"j")
    assert (session.row, session.col) == origin, "overridden j binding survived"
    session.send(b"z")
    assert session.row == origin[0] + 1
    session.send(b"\x0e")
    assert session.row == origin[0] + 1, "overridden action retained its readline alias"
    session.send(b"/main_target\r")
    session.wait_text("Search candidates")
    session.send(b"\r")
    session.wait_text("Captured old target.rs")
    session.send(b"gd")
    assert "Definitions candidates" not in session.text(), "overridden gd binding survived"
    session.send(b"gD")
    session.wait_text("Definitions candidates")
    session.send(b"x")
    session.wait_text("Files")
    assert "┌Diff" not in session.text()
    assert "x close" in session.text(), "footer ignored close override"
    session.send(b",q")
    session.finish()

    help_result = subprocess.run([str(binary), "--config", str(explicit_config), "--help"], env=env, capture_output=True, text=True, check=True)
    assert "close_diff_view: x\n" in help_result.stdout
    assert "cursor_down: z\n" in help_result.stdout
    assert "go_to_definition: gD\n" in help_result.stdout

    # An empty value disables its action; explicit config also wins discovery.
    explicit_config.write_text('[features]\nkeymap="vim"\nreadline=false\n[keybindings]\ncursor_down=""\n')
    session = Session([str(binary), "--config", str(explicit_config), "--", "HEAD"])
    session.wait_text("Files")
    session.send(b"\r")
    session.wait_text("Diff")
    origin = (session.row, session.col)
    session.send(b"j\x1b[B")
    assert (session.row, session.col) == origin, "disabled cursor_down action remained active"
    session.send(b",q")
    session.finish()

    for invalid in ['[features]\nkeymap="unknown"\n', '[features]\nreadline="yes"\n',
                    '[unknown]\nx=true\n', '[keybindings]\nunknown="j"\n',
                    '[keybindings]\ncursor_down="ctrl-unknown"\n',
                    '[keybindings]\ncursor_down="k"\n',
                    '[keybindings]\ncursor_down="g"\n',
                    '[keybindings]\ngo_to_definition="g esc"\n',
                    '[keybindings]\nopen_diff_view="ctrl-i"\n',
                    '[keybindings]\ngo_to_definition="ctrl-["\n']:
        explicit_config.write_text(invalid)
        result = subprocess.run([str(binary), "--config", str(explicit_config), "--", "HEAD"], cwd=repo, env=env, capture_output=True)
        assert result.returncode != 0 and b"review:" in result.stderr, (invalid, result.stderr)
        assert b"interactive terminal" not in result.stderr, "config was not validated before terminal setup"
    result = subprocess.run([str(binary), "--config", str(root / "missing.toml"), "--", "HEAD"], cwd=repo, env=env, capture_output=True)
    assert result.returncode != 0 and b"interactive terminal" not in result.stderr
    explicit_config.write_text('[keybindings]\ncursor_down=""\n')
    help_result = subprocess.run([str(binary), "--help", "--config", str(explicit_config), "--keymap", "vim"], env=env, capture_output=True, text=True, check=True)
    assert "cursor_down: (disabled)\n" in help_result.stdout
    assert "Active bindings (Vim)" in help_result.stdout
    config_file.unlink()
    assert config_before == {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    print("PTY passed: discovered/explicit config, Vim/Emacs, overrides, readline, disabled/removed keys and invalid config", flush=True)

    # Captured documentation preserves candidate origin and never writes marks.
    docs_before = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    for dismiss in [b"q", b"\x1b", b"\x0f"]:
        session = Session()
        session.wait_text("Files")
        session.send(b"/main_target\r")
        session.wait_text("Search candidates")
        session.send(b"K")
        session.wait_text("Captured terminal documentation.")
        session.send(b" ")
        session.resize(3, 8)
        session.resize(30, 140)
        session.send(dismiss)
        session.wait_text("Search candidates")
        assert session.process.poll() is None
        session.send(b"\r")
        session.wait_text("Captured old target.rs")
        session.send(b"K")
        session.wait_text("Function documentation")
        session.send(b"q")
        session.wait_text("Captured old target.rs")
        session.send(b"/caller\r")
        session.wait_text("Search candidates")
        session.send(b"\r")
        session.wait_text("Captured old caller.rs")
        session.send(b"014lK")
        session.wait_text("Function documentation")
        session.send(b"q")
        session.wait_text("Captured old caller.rs")
        session.send(b"K")
        session.wait_text("Captured terminal documentation.")
        session.send(b",q")
        session.finish()
    session = Session()
    session.wait_text("Files")
    session.send(b"/docs_caller\r")
    session.wait_text("Search candidates")
    session.send(b"\r")
    session.wait_text("Captured old docs.rs")
    session.send(b"019lK")
    session.wait_text("Documentation candidates")
    session.wait_text("2 results")
    session.send(b"j\r")
    session.wait_text("Documentation unavailable")
    session.send(b"k\r")
    session.wait_text("Captured ambiguous documentation.")
    session.send(b",q")
    session.finish()
    docs_config = root / "documentation.toml"
    for style in ["vim", "emacs"]:
        for binding in ["D", ""]:
            docs_config.write_text(f'[features]\nkeymap="{style}"\n[keybindings]\nshow_documentation="{binding}"\n')
            command = [str(binary), "--config", str(docs_config)]
            help_result = subprocess.run([*command, "--help"], env=env, capture_output=True, text=True, check=True)
            assert f"show_documentation: {binding or '(disabled)'}\n" in help_result.stdout
            session = Session([*command, "--", "HEAD"])
            session.wait_text("Files")
            session.send(b"/main_target\r")
            session.wait_text("Search candidates")
            origin = session.text()
            session.send(b"K")
            assert session.text() == origin, f"replaced default K remained active: {style}, {binding!r}"
            session.send(b"D")
            if binding:
                session.wait_text("Function documentation")
                session.wait_text("Captured terminal documentation.")
                session.send(b"q")
                session.wait_text("Search candidates")
            else:
                assert session.text() == origin, f"disabled documentation action ran: {style}"
            session.send(b",q")
            session.finish()
    docs_config.unlink()
    assert docs_before == {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    print("PTY passed: captured function documentation, custom/disabled keys in both styles, effective help, dismissal, tiny terminals and global quit", flush=True)

    # Definition previews use captured function/type targets and reversible views.
    preview_before = {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    preview_marks = {str(p.relative_to(root / "state")): p.read_bytes() for p in (root / "state").rglob("*") if p.is_file()}
    for symbol in ["preview_unique", "PreviewType"]:
        for dismiss in [b"q", b"\x1b", b"\x0f"]:
            session = Session()
            session.wait_text("Files")
            session.send(b"/preview_caller\r")
            session.wait_text("Search candidates")
            session.send(b"\r")
            session.wait_text("Captured old preview.rs")
            session.send(b"0" + str(preview_caller.index(symbol)).encode() + b"l")
            origin = (session.text(), session.row, session.col)
            session.send(b"P")
            session.wait_text("Definition preview")
            session.wait_text("Captured old preview.rs")
            session.send(b" ")  # Viewing never toggles the selected file's mark.
            session.resize(3, 8)
            session.resize(30, 140)
            session.send(dismiss)
            session.wait_text("Captured old preview.rs")
            assert (session.text(), session.row, session.col) == origin, "P dismissal lost source origin"
            session.send(b",q")
            session.finish()
    session = Session()
    session.wait_text("Files")
    session.send(b"/preview_ambiguous_caller\r")
    session.wait_text("Search candidates")
    session.send(b"\r")
    session.wait_text("Captured old preview.rs")
    session.send(b"0" + str(preview_ambiguous_caller.index("preview_ambiguous();")).encode() + b"lP")
    session.wait_text("PreviewDefinition candidates")
    session.wait_text("2 results")
    session.send(b"j")
    origin = (session.text(), session.row, session.col)
    session.send(b"\r")
    session.wait_text("Definition preview")
    session.send(b"q")
    session.wait_text("PreviewDefinition candidates")
    assert (session.text(), session.row, session.col) == origin, "P lost ambiguous candidate selection"
    session.send(b"q")
    session.wait_text("Captured old preview.rs")
    session.send(b",q")
    session.finish()
    preview_config = root / "preview.toml"
    for style in ["vim", "emacs"]:
        for binding in ["D", ""]:
            preview_config.write_text(f'[features]\nkeymap="{style}"\n[keybindings]\npreview_definition="{binding}"\n')
            command = [str(binary), "--config", str(preview_config)]
            help_result = subprocess.run([*command, "--help"], env=env, capture_output=True, text=True, check=True)
            assert f"preview_definition: {binding or '(disabled)'}\n" in help_result.stdout
            session = Session([*command, "--", "HEAD"])
            session.wait_text("Files")
            session.send(b"/PreviewType\r")
            session.wait_text("Search candidates")
            origin = (session.text(), session.row, session.col)
            session.send(b"P")
            assert (session.text(), session.row, session.col) == origin, "replaced default P remained active"
            session.send(b"D")
            if binding:
                session.wait_text("Definition preview")
                session.wait_text("Captured old preview.rs")
                session.send(b"q")
                session.wait_text("Search candidates")
                assert (session.text(), session.row, session.col) == origin, "P lost selected exact candidate"
            else:
                assert (session.text(), session.row, session.col) == origin, "disabled P action ran"
            session.send(b",q")
            session.finish()
    preview_config.unlink()
    session = Session()
    session.wait_text("Files")
    session.send(b"/preview_variable\r")
    session.wait_text("Search candidates")
    session.send(b"P")
    session.wait_text("Preview unavailable")
    session.wait_text("Search candidates")
    session.send(b",q")
    session.finish()
    assert preview_before == {str(p.relative_to(repo)): p.read_bytes() for p in repo.rglob("*") if p.is_file()}
    assert preview_marks == {str(p.relative_to(root / "state")): p.read_bytes() for p in (root / "state").rglob("*") if p.is_file()}
    print("PTY passed: captured function/type previews, exact return, ambiguity, unavailable targets, custom/disabled keys, read-only marks, tiny terminals and global quit", flush=True)

    # q closes a view first; leader-q exits directly from every context.
    for source_view in [False, True]:
        session = Session()
        session.wait_text("Files")
        if source_view:
            session.send(b"/main_target\r")
            session.wait_text("Search candidates")
            session.send(b"\r")
            session.wait_text("Captured old target.rs")
        else:
            session.send(b"\r")
            session.wait_text("Diff")
        session.send(b"q")
        session.wait_text("Files")
        assert "┌Diff" not in session.text() and "Captured old target.rs" not in session.text()
        assert session.process.poll() is None, "q exited before closing the view"
        session.send(b"q")
        session.finish()
    for context in ["files", "diff", "candidates", "source", "query", "original"]:
        command = [str(binary), "--", "--stat", "HEAD"] if context == "original" else None
        session = Session(command)
        session.wait_text("Original Git output" if context == "original" else "Files")
        if context == "diff":
            session.send(b"\r")
            session.wait_text("Diff")
        elif context in ("candidates", "source"):
            session.send(b"/main_target\r")
            session.wait_text("Search candidates")
            if context == "source":
                session.send(b"\r")
                session.wait_text("Captured old target.rs")
        elif context == "query":
            session.send(b"/main")
            session.wait_text("Search/outline: main")
        session.send(b",q")
        session.finish()
    print("PTY passed: q closes then exits; leader-q quits from files, diff, candidates, source, query and original output", flush=True)

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
        session.send(b",q")
        session.finish(expected)
        assert snapshot(cwd) == before, "installed review modified comparison inputs"

    # Previously omitted bindings and generic trait calls remain navigable even
    # when non-code files sort before unchanged source in the captured catalog.
    indexing_repo = root / "indexing"
    indexing_repo.mkdir()
    def indexing_git(*words):
        return subprocess.check_output([git, *words], cwd=indexing_repo, env=env, stderr=subprocess.STDOUT)
    indexing_git("init", "-q")
    indexing_git("config", "user.name", "Index acceptance")
    indexing_git("config", "user.email", "index@example.invalid")
    docs = indexing_repo / "a-docs"
    docs.mkdir()
    for index in range(1030):
        (docs / f"{index:04}.md").write_text("Documentation only.\n" * 460)
    trait_source = '''pub trait Convert<T> {
    fn adapt<U>(&self, input: U) -> T;
}
pub struct Worker;
impl Convert<u32> for Worker {
    fn adapt<U>(&self, _input: U) -> u32 { 7 }
}
pub fn convert_value<U>(value: U) -> U { value }
pub fn invoke(worker: Worker) -> u32 {
    let local_value = 3;
    let callback = convert_value::<u32>;
    let after = callback(local_value);
    // adapt local_value convert_value
    let ignored = "adapt local_value convert_value";
    worker.adapt::<u32>(after)
}
'''
    (indexing_repo / "zzz-traits.rs").write_text(trait_source)
    (indexing_repo / "caller.rs").write_text("fn caller() { invoke(Worker); }\n")
    indexing_git("add", ".")
    indexing_git("commit", "-qm", "generic trait and late source")
    (indexing_repo / "caller.rs").write_text("fn caller() { let result = invoke(Worker); }\n")
    indexing_before = snapshot(indexing_repo)
    index_state_before = snapshot(root / "state")
    session = Session([str(binary), "--", "HEAD", "--", "caller.rs"], cwd=indexing_repo)
    closed(session)
    session.send(b"/adapt\r")
    session.wait_text("4 results")  # trait declaration and impl, on both sides
    session.send(b"\r")
    session.wait_text("Captured old zzz-traits.rs")
    def index_position(text):
        for row, line in enumerate(session.text().splitlines()):
            if text in line:
                return line.index(text), row
        raise AssertionError((text, session.text()))
    symbol_x, symbol_y = index_position("adapt<U>")
    assert session.colors[(symbol_y, symbol_x)] is not None, "generic trait declaration lost highlighting"
    session.send(b"gr")
    session.wait_text("1 results")  # excludes same text in comment and string
    session.send(b"\r")
    session.wait_text("Captured old zzz-traits.rs")
    symbol_x, symbol_y = index_position("adapt::<u32>")
    assert (session.row, session.col) == (symbol_y, symbol_x), "generic call opened at wrong scalar position"
    read_cursor = (session.row, session.col)
    session.send(b"gd")
    session.wait_text("2 results")  # ambiguity retains trait declaration and impl
    session.send(b"\x0f")
    session.wait_text("Captured old zzz-traits.rs")
    assert (session.row, session.col) == read_cursor
    session.send(b"]")
    session.wait_text("Captured new zzz-traits.rs")
    session.send(b"gd")
    session.wait_text("2 results")
    session.send(b"\x1b")
    closed(session)

    for symbol, occurrence in [("local_value", "local_value);"), ("convert_value", "convert_value::<u32>")]:
        session.send(b"/" + symbol.encode() + b"\r")
        session.wait_text("2 results")
        session.send(b"\r")
        session.wait_text("Captured old zzz-traits.rs")
        session.send(b"gr")
        session.wait_text("1 results")
        session.send(b"\r")
        session.wait_text("Captured old zzz-traits.rs")
        symbol_x, symbol_y = index_position(occurrence)
        assert (session.row, session.col) == (symbol_y, symbol_x), (symbol, session.text())
        session.send(b"gd")
        session.wait_text("1 results")
        session.send(b"\x1b")
        closed(session)
    assert "Partial name/syntax" not in session.text()
    session.send(b",q")
    session.finish()
    assert snapshot(indexing_repo) == indexing_before, "indexing/navigation changed repository bytes"
    assert snapshot(root / "state") == index_state_before, "indexing/navigation implicitly changed marks"
    print("PTY passed: late unchanged sources, generic trait declarations/turbofish calls, local bindings, function references and exact read-only navigation", flush=True)

    # R13: installed cursor/mouse behavior, independently captured immutable inputs.
    mouse_repo = root / "cursor-mouse"
    mouse_repo.mkdir()
    def mouse_git(*words):
        return subprocess.check_output([git, *words], cwd=mouse_repo, env=env, stderr=subprocess.STDOUT)
    mouse_git("init", "-q")
    mouse_git("config", "user.name", "Cursor acceptance")
    mouse_git("config", "user.email", "cursor@example.invalid")
    for index in range(15):
        (mouse_repo / f"a{index:02}.rs").write_text(f"\tfn old_symbol_{index:02}() {{}}\n" + "\t// 界e\u0301\\ old\n" * 45)
    mouse_git("add", ".")
    mouse_git("commit", "-qm", "captured old sides")
    for index in range(15):
        (mouse_repo / f"a{index:02}.rs").write_text(f"\tfn new_symbol_{index:02}() {{}}\n" + "\t// 界e\u0301\\ new\n" * 45)
    mouse_before = snapshot(mouse_repo)
    state_before_mouse = snapshot(root / "state")
    session = Session([str(binary), "--", "HEAD"], cwd=mouse_repo)
    closed(session)
    def pointer(code, column, row, release=False):
        return f"\x1b[<{code};{column+1};{row+1}{'m' if release else 'M'}".encode()
    def locate(text):
        for row, line in enumerate(session.text().splitlines()):
            if text in line:
                return line.index(text), row
        raise AssertionError((text, session.text()))
    # File wheel scrolling does not select: Enter still opens the initial file.
    session.resize(12, 140)
    session.send(pointer(65, 2, 2))
    closed(session)
    session.send(b"\r")
    session.wait_text("old_symbol_00")
    session.resize(30, 140)
    assert snapshot(root / "state") == state_before_mouse
    # Select a visible file once: preview closes. Two same-file presses open it.
    x, y = locate("a03.rs")
    session.send(pointer(0, x, y) + pointer(0, x, y, True))
    closed(session)
    session.pump(0.5)
    session.send(pointer(0, x, y) + pointer(0, x, y, True) + pointer(0, x, y) + pointer(0, x, y, True))
    session.wait_text("old_symbol_03")
    # Scroll to the added rows, then click the exact transformed/tabbed symbol.
    # New rows follow 46 removed rows; Vim counts reach them independently of pane height.
    session.send(b"gg60j")
    session.wait_text("new_symbol_03")
    x, y = locate("new_symbol_03")
    session.send(pointer(0, x + 2, y) + pointer(0, x + 2, y, True) + b"gd")
    session.wait_text('Definitions candidates for "new_symbol_03"')
    session.wait_text("1 results")
    session.send(b"\r")
    session.wait_text("Captured new a03.rs")
    assert "Partial name/syntax" not in session.text()
    assert "never binding" not in session.text()
    symbol_x, symbol_y = locate("new_symbol_03")
    assert session.colors[(symbol_y, symbol_x)] is not None, "jump target lost syntax highlighting"
    cursor = (session.row, session.col)
    session.send(b"l")
    assert (session.row, session.col) == (cursor[0], cursor[1] + 1), (cursor, session.row, session.col)
    session.send(b"h")
    assert (session.row, session.col) == cursor
    # Vim * searches this captured buffer, highlights matches, wraps and repeats.
    session.send(b"G$*")
    session.wait_text("/new · 1/45")
    first_match = (session.row, session.col)
    session.send(b"n")
    session.wait_text("/new · 2/45")
    assert (session.row, session.col) == (first_match[0] + 1, first_match[1])
    session.send(b"N")
    assert (session.row, session.col) == first_match
    session.send(b"gg0ww")
    assert (session.row, session.col) == cursor
    session.send(b"gd")
    session.wait_text('Definitions candidates for "new_symbol_03"')
    session.send(b"\x0f")
    session.wait_text("Captured new a03.rs")
    assert (session.row, session.col) == cursor, "back did not restore the cursor"
    session.send(b"[gd")
    session.wait_text('Definitions candidates for "old_symbol_03"')
    session.send(b"\x0f")
    session.wait_text("Captured old a03.rs")
    # Wheel the preview while files hold focus, then move files to prove focus stayed.
    session.send(b"\t")
    session.send(pointer(65, 80, 5))
    session.send(b"j")
    closed(session)
    # Reviewed files have the same single/double click opening contract.
    session.send(b" ")
    session.wait_text("Review mark saved")
    state_after_mark = snapshot(root / "state")
    x, y = locate("a04.rs")
    session.send(pointer(0, x, y) + pointer(0, x, y, True))
    closed(session)
    session.pump(0.5)
    session.send(pointer(0, x, y) + pointer(0, x, y, True) + pointer(0, x, y) + pointer(0, x, y, True))
    session.wait_text("Diff")
    session.send(b"\x1b[6~\x1b[5~")
    session.send(b"\x1b")
    closed(session)
    assert session.process.poll() is None, "Escape exited instead of closing the diff"
    session.send(b"\x1b")
    assert session.process.poll() is None, "Escape exited from the file list"
    session.send(b"\r")
    session.wait_text("Diff")
    for shape in [(1, 1), (3, 8), (30, 140)]:
        session.resize(*shape)
    session.send(b",q")
    session.finish()
    assert snapshot(mouse_repo) == mouse_before, "cursor/mouse review changed repository bytes"
    assert snapshot(root / "state") == state_after_mark, "cursor/mouse input changed marks"
    print("PTY passed: R13 exact cursor navigation, file clicks, wheels, history, side switching, resize and read-only state", flush=True)

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
    session.send(b"\x1b")
    session.send(b" ")
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
            session.send(b"/")
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

print("PTY passed: installed language navigation, terminal lifecycle, R1-R13 workflow, branch/ref/staged/worktree/no-index invalidation, Git compatibility and original-output fallback" + (", injected panic/I/O failure" if args.test_binary else ""))
