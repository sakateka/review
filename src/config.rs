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
    pub telemetry: Telemetry,
    pub keyboard: Keyboard,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keyboard {
    pub layouts: Vec<String>,
    pub langmap: String,
}
impl Default for Keyboard {
    fn default() -> Self {
        Self {
            layouts: crate::layouts::DEFAULT
                .iter()
                .map(|s| s.to_string())
                .collect(),
            langmap: String::new(),
        }
    }
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
    pub keymap: Keymap,
    pub readline: bool,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub colorscheme: Option<String>,
    pub cursorline: bool,
    pub wrap: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            colorscheme: None,
            cursorline: true,
            wrap: false,
        }
    }
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Telemetry {
    pub enabled: bool,
}
impl Default for Telemetry {
    fn default() -> Self {
        Self { enabled: true }
    }
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
        let default = Self::default_path();
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
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .map(|p| p.join("review/config.toml"))
    }
    pub fn save_setting(path: &Path, key: &str, value: toml_edit::Item) -> Result<()> {
        use std::io::Write;
        let resolved;
        let path = if path.is_symlink() {
            resolved = path
                .canonicalize()
                .context("resolve configuration symlink")?;
            resolved.as_path()
        } else {
            path
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read configuration {}", path.display()));
            }
        };
        // Never overwrite a configuration that the application cannot read.
        Self::parse(&text)?;
        let mut document = text
            .parse::<toml_edit::DocumentMut>()
            .context("edit configuration")?;
        if !document.contains_key("appearance") {
            document["appearance"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        document["appearance"][key] = value;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            temp.as_file().set_permissions(metadata.permissions())?;
        }
        temp.write_all(document.to_string().as_bytes())?;
        temp.persist(path).map_err(|error| error.error)?;
        Ok(())
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
        let langmap = crate::layouts::langmap(&self.keyboard.layouts, &self.keyboard.langmap)?;
        Bindings::new(
            cli.unwrap_or(self.features.keymap),
            self.features.readline,
            &self.keybindings,
        )
        .map(|bindings| bindings.with_langmap(langmap))
    }
}
