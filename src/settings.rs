use anyhow::{Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShogunDesktopSettings {
    #[serde(default)]
    pub ssh: SshSettings,
    #[serde(default)]
    pub project: ProjectSettings,
    #[serde(default)]
    pub sessions: SessionSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshSettings {
    #[serde(default = "default_ssh_host")]
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSettings {
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettings {
    #[serde(default = "default_shogun_session")]
    pub shogun: String,
    #[serde(default = "default_multiagent_session")]
    pub multiagent: String,
}

fn default_ssh_host() -> String {
    "localhost".into()
}

fn default_ssh_port() -> u16 {
    22
}

fn default_shogun_session() -> String {
    "shogun".into()
}

fn default_multiagent_session() -> String {
    "multiagent".into()
}

impl Default for ShogunDesktopSettings {
    fn default() -> Self {
        Self {
            ssh: SshSettings::default(),
            project: ProjectSettings::default(),
            sessions: SessionSettings::default(),
        }
    }
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            host: default_ssh_host(),
            port: default_ssh_port(),
            user: String::new(),
            key_path: String::new(),
            password: String::new(),
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            shogun: default_shogun_session(),
            multiagent: default_multiagent_session(),
        }
    }
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shogun-desktop")
        .join("settings.toml")
}

pub fn load_settings() -> Result<ShogunDesktopSettings> {
    let path = settings_path();
    if !path.exists() {
        return Ok(ShogunDesktopSettings::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let settings = toml::from_str(&raw).context("parse settings.toml")?;
    Ok(settings)
}

pub fn save_settings(settings: &ShogunDesktopSettings) -> Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(settings)?;
    fs::write(&path, raw).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
