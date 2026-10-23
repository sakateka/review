//! A review process in a real pseudo-terminal plus the minimal terminal
//! emulator the smoke test asserts against.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

/// A cell foreground: an indexed palette entry or a 24-bit RGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fg {
    Indexed(i64),
    Rgb(i64, i64, i64),
}

/// `None` is the terminal default foreground.
pub type Color = Option<Fg>;

pub const fn idx(index: i64) -> Color {
    Some(Fg::Indexed(index))
}

pub const fn rgb(red: i64, green: i64, blue: i64) -> Color {
    Some(Fg::Rgb(red, green, blue))
}

/// Environment shared by every child: the inherited environment with
/// overrides applied and `NO_COLOR` removed.
#[derive(Clone)]
pub struct TestEnv {
    pub vars: Vec<(OsString, OsString)>,
}

impl TestEnv {
    pub fn apply(&self, command: &mut Command) {
        for (key, value) in &self.vars {
            command.env(key, value);
        }
        // The PTY models a color-capable terminal.
        command.env_remove("NO_COLOR");
    }
}

/// The termios fields Python's `termios.tcgetattr` reports.
#[derive(Debug, PartialEq, Eq)]
pub struct Termios {
    iflag: libc::tcflag_t,
    oflag: libc::tcflag_t,
    cflag: libc::tcflag_t,
    lflag: libc::tcflag_t,
    ispeed: libc::speed_t,
    ospeed: libc::speed_t,
    cc: Vec<libc::cc_t>,
}

fn termios_of(fd: RawFd) -> Termios {
    let mut raw: libc::termios = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::tcgetattr(fd, &mut raw) };
    assert_eq!(
        result,
        0,
        "tcgetattr failed: {}",
        io::Error::last_os_error()
    );
    Termios {
        iflag: raw.c_iflag,
        oflag: raw.c_oflag,
        cflag: raw.c_cflag,
        lflag: raw.c_lflag,
        ispeed: unsafe { libc::cfgetispeed(&raw) },
        ospeed: unsafe { libc::cfgetospeed(&raw) },
        cc: raw.c_cc.to_vec(),
    }
}

fn set_window_size(fd: RawFd, rows: u16, cols: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) };
    assert_eq!(
        result,
        0,
        "TIOCSWINSZ failed: {}",
        io::Error::last_os_error()
    );
}

fn set_cloexec(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD failed: {}", io::Error::last_os_error());
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    assert!(
        result >= 0,
        "F_SETFD failed: {}",
        io::Error::last_os_error()
    );
}

fn stdio_for(fd: RawFd) -> Stdio {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    assert!(copy >= 0, "dup failed: {}", io::Error::last_os_error());
    Stdio::from(unsafe { File::from_raw_fd(copy) })
}

fn write_all(fd: RawFd, mut data: &[u8]) {
    while !data.is_empty() {
        let written = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            panic!("PTY write failed: {error}");
        }
        data = &data[written as usize..];
    }
}

/// Python's `returncode`: the exit code, or the negated terminating signal.
fn return_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().expect("status without code or signal"))
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

pub fn tail(bytes: &[u8], count: usize) -> String {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(count)..]).into_owned()
}

/// Character (not byte) index of `needle` in `line`; screen columns map to
/// characters one to one because `text()` renders one character per cell.
pub fn char_index(line: &str, needle: &str) -> Option<usize> {
    line.find(needle).map(|byte| line[..byte].chars().count())
}

/// Every regular file below `path`, keyed by its relative path; an absent
/// directory has no files.
pub fn snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => panic!("read_dir {}: {error}", directory.display()),
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(base, &path, files);
            } else if path.is_file() {
                let relative = path.strip_prefix(base).unwrap();
                files.insert(
                    relative.to_string_lossy().into_owned(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(path, path, &mut files);
    files
}

pub struct Session {
    /// Like a real terminal, answer primary device attributes; answer the
    /// kitty keyboard-protocol query only when modelling kitty.
    kitty: bool,
    master: Option<OwnedFd>,
    slave: Option<OwnedFd>,
    pub original: Termios,
    process: Child,
    pub output: Vec<u8>,
    utf8: Vec<u8>,
    pending: String,
    pub cells: HashMap<(i64, i64), char>,
    pub colors: HashMap<(i64, i64), Color>,
    foreground: Color,
    pub row: i64,
    pub col: i64,
    pub cursor_visible: bool,
    pub columns: i64,
}

impl Session {
    pub fn new(
        program: &Path,
        args: &[&str],
        cwd: &Path,
        env: &TestEnv,
        scenario: Option<&str>,
        kitty: bool,
    ) -> Session {
        let (mut master, mut slave) = (-1, -1);
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "openpty failed: {}", io::Error::last_os_error());
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        set_cloexec(master.as_raw_fd());
        set_cloexec(slave.as_raw_fd());
        let original = termios_of(slave.as_raw_fd());
        set_window_size(slave.as_raw_fd(), 30, 140);
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd);
        env.apply(&mut command);
        if let Some(scenario) = scenario {
            command.env("REVIEW_TERMINAL_SCENARIO", scenario);
        }
        command
            .stdin(stdio_for(slave.as_raw_fd()))
            .stdout(stdio_for(slave.as_raw_fd()))
            .stderr(stdio_for(slave.as_raw_fd()));
        // A new session whose controlling terminal is the PTY slave (stdin).
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {} failed: {error}", program.display()));
        Session {
            kitty,
            master: Some(master),
            slave: Some(slave),
            original,
            process,
            output: Vec::new(),
            utf8: Vec::new(),
            pending: String::new(),
            cells: HashMap::new(),
            colors: HashMap::new(),
            foreground: None,
            row: 0,
            col: 0,
            cursor_visible: true,
            columns: 140,
        }
    }

    fn master_fd(&self) -> RawFd {
        self.master
            .as_ref()
            .expect("PTY already closed")
            .as_raw_fd()
    }

    fn slave_fd(&self) -> RawFd {
        self.slave.as_ref().expect("PTY already closed").as_raw_fd()
    }

    pub fn termios(&self) -> Termios {
        termios_of(self.slave_fd())
    }

    pub fn pump(&mut self, seconds: f64) {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            let now = Instant::now();
            if now >= end {
                break;
            }
            let remaining = (end - now).as_micros().div_ceil(1000) as libc::c_int;
            let mut poll = libc::pollfd {
                fd: self.master_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, remaining) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::Interrupted,
                    "poll failed: {error}"
                );
                continue;
            }
            if ready == 0 {
                continue;
            }
            let mut buffer = vec![0u8; 65536];
            let count =
                unsafe { libc::read(self.master_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::Interrupted,
                    "PTY read failed: {error}"
                );
                continue;
            }
            let data = &buffer[..count as usize];
            self.output.extend_from_slice(data);
            let text = self.decode(data);
            self.feed(&text);
        }
    }

    /// Incremental UTF-8 decoding with replacement characters.
    fn decode(&mut self, data: &[u8]) -> String {
        self.utf8.extend_from_slice(data);
        let mut text = String::new();
        let mut rest: &[u8] = &self.utf8;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap());
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{FFFD}');
                            rest = &after[length..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.utf8 = rest.to_vec();
        text
    }

    fn feed(&mut self, text: &str) {
        self.pending.push_str(text);
        let chars: Vec<char> = self.pending.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '\x1b' {
                match parse_csi(&chars[index..]) {
                    Some((parameters, command, length)) => {
                        self.csi(&parameters, command);
                        index += length;
                    }
                    None => {
                        let rest = &chars[index..];
                        if rest.len() < 2 || rest[1] == '[' {
                            break;
                        }
                        index += 2;
                    }
                }
                continue;
            }
            let ch = chars[index];
            index += 1;
            if ch == '\r' {
                self.col = 0;
            } else if ch == '\n' {
                self.row += 1;
            } else if ch >= ' ' {
                let width = ch.width().unwrap_or(1) as i64;
                self.cells.insert((self.row, self.col), ch);
                self.colors.insert((self.row, self.col), self.foreground);
                self.col += width;
            }
        }
        self.pending = chars[index..].iter().collect();
    }

    fn csi(&mut self, parameters: &str, command: char) {
        if command == 'u' && parameters == "?" && self.kitty {
            write_all(self.master_fd(), b"\x1b[?0u");
        } else if command == 'c' && parameters.is_empty() {
            write_all(self.master_fd(), b"\x1b[?62;22c");
        }
        let numbers: Vec<i64> = parameters
            .trim_start_matches(['?', '<', '>', '='])
            .split(';')
            .map(|number| {
                if number.is_empty() {
                    0
                } else {
                    number
                        .parse()
                        .unwrap_or_else(|_| panic!("bad CSI parameter {parameters:?}"))
                }
            })
            .collect();
        let value = numbers.first().copied().unwrap_or(0);
        let or_one = if value == 0 { 1 } else { value };
        match command {
            'm' => {
                let mut i = 0;
                while i < numbers.len() {
                    let n = numbers[i];
                    if n == 0 || n == 39 {
                        self.foreground = None;
                    } else if (30..=37).contains(&n) {
                        self.foreground = idx(n - 30);
                    } else if (90..=97).contains(&n) {
                        self.foreground = idx(n - 90 + 8);
                    } else if (n == 38 || n == 48) && i + 2 < numbers.len() && numbers[i + 1] == 5 {
                        if n == 38 {
                            self.foreground = idx(numbers[i + 2]);
                        }
                        i += 2;
                    } else if (n == 38 || n == 48) && i + 4 < numbers.len() && numbers[i + 1] == 2 {
                        if n == 38 {
                            self.foreground = rgb(numbers[i + 2], numbers[i + 3], numbers[i + 4]);
                        }
                        i += 4;
                    }
                    i += 1;
                }
            }
            'H' | 'f' => {
                self.row = (value - 1).max(0);
                self.col = (numbers.get(1).copied().unwrap_or(1) - 1).max(0);
            }
            'A' => self.row = (self.row - or_one).max(0),
            'B' => self.row += or_one,
            'C' => self.col += or_one,
            'D' => self.col = (self.col - or_one).max(0),
            'G' => self.col = (value - 1).max(0),
            'J' if value == 2 || value == 3 => self.cells.clear(),
            'K' => {
                let (row, col) = (self.row, self.col);
                self.cells
                    .retain(|&(cell_row, cell_col), _| cell_row != row || cell_col < col);
            }
            'h' if parameters == "?1049" => self.cells.clear(),
            'h' | 'l' if parameters == "?25" => self.cursor_visible = command == 'h',
            _ => {}
        }
    }

    pub fn text(&self) -> String {
        let width = self.columns.max(160);
        (0..50)
            .map(|row| {
                (0..width)
                    .map(|col| self.cells.get(&(row, col)).copied().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn lines(&self) -> Vec<String> {
        self.text().split('\n').map(str::to_owned).collect()
    }

    pub fn color(&self, row: usize, col: usize) -> Color {
        *self
            .colors
            .get(&(row as i64, col as i64))
            .unwrap_or_else(|| panic!("no color at {row},{col}\n{}", self.text()))
    }

    pub fn wait_text(&mut self, text: &str) {
        self.wait_text_for(text, 10.0);
    }

    pub fn wait_text_for(&mut self, text: &str, timeout: f64) {
        let end = Instant::now() + Duration::from_secs_f64(timeout);
        while !self.text().contains(text) {
            self.pump(0.1);
            assert!(
                Instant::now() < end,
                "timed out waiting for {text:?}\n{}\n{:?}",
                self.text(),
                tail(&self.output, 500)
            );
        }
    }

    pub fn send(&mut self, data: &[u8]) {
        write_all(self.master_fd(), data);
        self.pump(0.2);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.columns = i64::from(cols);
        set_window_size(self.slave_fd(), rows, cols);
        self.signal(libc::SIGWINCH);
        self.pump(0.2);
    }

    pub fn signal(&self, signal: libc::c_int) {
        let result = unsafe { libc::kill(self.process.id() as libc::pid_t, signal) };
        assert_eq!(result, 0, "kill failed: {}", io::Error::last_os_error());
    }

    pub fn id(&self) -> u32 {
        self.process.id()
    }

    /// `None` while the process runs, else its return code.
    pub fn poll(&mut self) -> Option<i32> {
        self.process.try_wait().unwrap().map(return_code)
    }

    pub fn finish(&mut self, expected: i32) {
        let end = Instant::now() + Duration::from_secs(10);
        let code = loop {
            if let Some(code) = self.poll() {
                break code;
            }
            assert!(
                Instant::now() < end,
                "process did not exit\n{}\n{:?}",
                self.text(),
                tail(&self.output, 5000)
            );
            self.pump(0.05);
        };
        self.pump(0.2);
        assert_eq!(code, expected, "{:?}", tail(&self.output, 5000));
        assert!(self.termios() == self.original, "PTY termios not restored");
        assert!(
            contains(&self.output, b"\x1b[?1049l"),
            "alternate screen not restored"
        );
        assert!(contains(&self.output, b"\x1b[?25h"), "cursor not restored");
        for mode in [1000, 1002, 1003, 1006, 1015] {
            assert!(
                contains(&self.output, format!("\x1b[?{mode}h").as_bytes()),
                "mouse capture not enabled: {mode}"
            );
            assert!(
                contains(&self.output, format!("\x1b[?{mode}l").as_bytes()),
                "mouse capture not restored: {mode}"
            );
        }
        self.cleanup();
    }

    fn cleanup(&mut self) {
        if matches!(self.process.try_wait(), Ok(None)) {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
        self.master = None;
        self.slave = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Match `ESC [ params intermediates final` at the start of `chars`, returning
/// the parameters, final byte and sequence length.
fn parse_csi(chars: &[char]) -> Option<(String, char, usize)> {
    if chars.get(1) != Some(&'[') {
        return None;
    }
    let mut end = 2;
    while end < chars.len() && ('0'..='?').contains(&chars[end]) {
        end += 1;
    }
    let parameters: String = chars[2..end].iter().collect();
    while end < chars.len() && (' '..='/').contains(&chars[end]) {
        end += 1;
    }
    let command = *chars.get(end)?;
    ('@'..='~')
        .contains(&command)
        .then_some((parameters, command, end + 1))
}
