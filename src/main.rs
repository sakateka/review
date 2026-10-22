mod app;
mod config;
mod git;
mod keybindings;
mod motion;
mod navigation;
mod preview;
mod search;
mod source;
mod state;
mod ui;

use anyhow::{Result, bail};
use std::{ffi::OsString, io::IsTerminal};

const HELP: &str = "review [--help | --version] [--keymap vim|emacs] [--config PATH] -- [git diff arguments]\n\nReview a captured Git comparison. Git's own -- path separator is preserved.\nConfigure [features] keymap/readline and [keybindings] action=\"key\" in --config\nor $XDG_CONFIG_HOME/review/config.toml (fallback $HOME/.config/review/config.toml).\nCLI keymap overrides config. Overrides replace all defaults; \"\" disables an action.\nKeys: characters, named keys (enter, esc, space, down), ctrl-/alt- chords, sequences (gd).\n\n";

fn help(keymap: Option<keybindings::Keymap>, config: Option<&std::path::Path>) -> Result<String> {
    let bindings = config::Config::load(config)?.bindings(keymap)?;
    let style = match bindings.keymap {
        keybindings::Keymap::Vim => "Vim",
        keybindings::Keymap::Emacs => "Emacs",
    };
    let mut text = format!("{HELP}Active bindings ({style}):\n{}", bindings.help());
    if bindings.keymap == keybindings::Keymap::Vim {
        text.push_str("Vim counts repeat configured motions and line jumps.\n");
    }
    text.push_str("\nSymbol query: Enter submits, Escape returns, Backspace deletes.\nEscape cancels pending sequences/counts. Ctrl-C safely exits.\nClick selects/positions; double-click opens; wheel scrolls.\nMarks are saved under $XDG_STATE_HOME/review, or $HOME/.local/state/review.\nReviewing requires system Git and an interactive terminal.\n");
    Ok(text)
}

#[derive(Debug, PartialEq)]
enum Cli {
    Help {
        keymap: Option<keybindings::Keymap>,
        config: Option<std::path::PathBuf>,
    },
    Version,
    Review {
        args: Vec<OsString>,
        keymap: Option<keybindings::Keymap>,
        config: Option<std::path::PathBuf>,
    },
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli> {
    let mut args = args.into_iter();
    let mut keymap = None;
    let mut config = None;
    let mut help = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                help = true;
            }
            Some("--version" | "-V") => {
                if args.next().is_some() {
                    bail!("--version takes no arguments");
                }
                return Ok(Cli::Version);
            }
            Some("--keymap") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--keymap requires vim or emacs"))?;
                keymap = Some(keybindings::Keymap::parse(value.to_str().unwrap_or(""))?);
            }
            Some("--config") => {
                config = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--config requires a path"))?
                        .into(),
                );
            }
            Some("--") => {
                if help {
                    bail!("--help does not accept Git arguments");
                }
                return Ok(Cli::Review {
                    args: args.collect(),
                    keymap,
                    config,
                });
            }
            _ => bail!("place Git diff arguments after review's --; see review --help"),
        }
    }
    if help {
        Ok(Cli::Help { keymap, config })
    } else {
        Ok(Cli::Review {
            args: Vec::new(),
            keymap,
            config,
        })
    }
}

fn run() -> Result<i32> {
    let (args, keymap, config) = match parse_args(std::env::args_os().skip(1))? {
        Cli::Help { keymap, config } => {
            print!("{}", help(keymap, config.as_deref())?);
            return Ok(0);
        }
        Cli::Version => {
            println!("review {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Cli::Review {
            args,
            keymap,
            config,
        } => (args, keymap, config),
    };
    let bindings = config::Config::load(config.as_deref())?.bindings(keymap)?;
    let cwd = std::env::current_dir()?;
    let capture = git::capture(&cwd, &args)?;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("review requires an interactive terminal (use git diff for piped output)");
    }
    let store = state::Store::for_session(&capture.cwd, &capture.args)?;
    let mut app = app::App::new(capture, store);
    app.bindings = bindings;
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
