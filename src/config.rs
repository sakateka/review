use crate::keybindings::{Bindings, Keymap};
use crate::theme::Theme;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub features: Features,
    pub appearance: Appearance,
    pub keybindings: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
    pub keymap: Keymap,
    pub readline: bool,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub colorscheme: Option<String>,
}
impl Default for Features {
    fn default() -> Self {
        Self {
            keymap: Keymap::Vim,
            readline: true,
        }
    }
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("invalid review configuration")
    }
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let default = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .map(|p| p.join("review/config.toml"));
        let Some(path) = explicit.or(default.as_deref()) else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("configuration {}", path.display()))
            }
            Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("read configuration {}", path.display()))
            }
        }
    }
    /// The startup theme; an unknown name is a configuration error.
    pub fn theme(&self) -> Result<Theme> {
        let Some(name) = &self.appearance.colorscheme else {
            return Ok(Theme::default());
        };
        Theme::named(name).with_context(|| {
            format!(
                "unknown [appearance] colorscheme {name:?}; expected one of: {}",
                Theme::names().join(", ")
            )
        })
    }
    pub fn bindings(&self, cli: Option<Keymap>) -> Result<Bindings> {
        Bindings::new(
            cli.unwrap_or(self.features.keymap),
            self.features.readline,
            &self.keybindings,
        )
    }
}
