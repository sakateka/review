mod app;
mod git;
mod motion;
mod navigation;
mod preview;
mod search;
mod source;
mod state;
mod ui;

use anyhow::{Result, bail};
use std::{ffi::OsString, io::IsTerminal};

const HELP: &str = "review [--help | --version] -- [git diff arguments]\n\nReview a captured Git comparison. Git's own -- path separator is preserved.\n\nControls: hjkl/arrows move; w/b/e and W/B/E move by words; 0/^/$ move within a line;\ngg/G jump to first/last line; counts work (10j, 3w, 20gg).\n* searches the word in this buffer; # searches backward; n/N repeats.\nCtrl-U/D scroll half pages; Ctrl-B/F or PageUp/PageDown scroll pages.\nEnter opens; Escape closes the open view; Tab switches panes; q quits.\n/ searches symbols; gd/gr finds definitions/references; Ctrl-O goes back;\n[ and ] choose old/new side; ? in candidates shows analysis details.\nClick selects/positions; double-click opens; wheel scrolls.\nSpace toggles and immediately saves the selected file's review mark.\n\nMarks are saved under $XDG_STATE_HOME/review, or $HOME/.local/state/review.\nRequires system Git and an interactive terminal.\n";

#[derive(Debug, PartialEq)]
enum Cli {
    Help,
    Version,
    Review(Vec<OsString>),
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None => Ok(Cli::Review(Vec::new())),
        Some(s) if s == "--help" || s == "-h" => {
            if args.next().is_some() {
                bail!("--help takes no arguments");
            }
            Ok(Cli::Help)
        }
        Some(s) if s == "--version" || s == "-V" => {
            if args.next().is_some() {
                bail!("--version takes no arguments");
            }
            Ok(Cli::Version)
        }
        Some(s) if s == "--" => Ok(Cli::Review(args.collect())),
        Some(_) => bail!("place Git diff arguments after review's --; see review --help"),
    }
}

fn run() -> Result<i32> {
    let args = match parse_args(std::env::args_os().skip(1))? {
        Cli::Help => {
            print!("{HELP}");
            return Ok(0);
        }
        Cli::Version => {
            println!("review {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Cli::Review(args) => args,
    };
    let cwd = std::env::current_dir()?;
    let capture = git::capture(&cwd, &args)?;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("review requires an interactive terminal (use git diff for piped output)");
    }
    let store = state::Store::for_session(&capture.cwd, &capture.args)?;
    let mut app = app::App::new(capture, store);
    ui::run(&mut app)?;
    Ok(app.capture.status.code().unwrap_or(1))
}

fn main() {
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("review: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests;
