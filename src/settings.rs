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
    #[serde(default)]
    pub terminal: TerminalSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSettings {
    #[serde(default = "default_terminal_font")]
    pub font: String,
}

fn default_terminal_font() -> String {
    "Moralerspace Neon HW".to_string()
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font: default_terminal_font(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlPathType {
    #[default]
    Socket,
    NamedPipe,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionBackend {
    #[default]
    System,
    Native,
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
    #[serde(default)]
    pub control_path: ControlPathType,
    #[serde(default)]
    pub connection_backend: ConnectionBackend,
    #[serde(default)]
    pub proxy_command: String,
    #[serde(default)]
    pub accept_all_host_keys: bool,
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
    #[serde(default = "default_agents")]
    pub agents: Vec<String>,
}

fn default_agents() -> Vec<String> {
    vec![
        "karo".into(),
        "gunshi".into(),
        "ashigaru1".into(),
        "ashigaru2".into(),
        "ashigaru3".into(),
        "ashigaru4".into(),
        "ashigaru5".into(),
        "ashigaru6".into(),
        "ashigaru7".into(),
    ]
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
            terminal: TerminalSettings::default(),
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
            control_path: ControlPathType::default(),
            connection_backend: ConnectionBackend::default(),
            proxy_command: String::new(),
            accept_all_host_keys: false,
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            shogun: default_shogun_session(),
            multiagent: default_multiagent_session(),
            agents: default_agents(),
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
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let settings = toml::from_str(&raw).context("parse settings.toml")?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_path_type_serde_roundtrip() {
        for variant in [
            ControlPathType::Socket,
            ControlPathType::NamedPipe,
            ControlPathType::None,
        ] {
            let settings = ShogunDesktopSettings {
                ssh: SshSettings {
                    control_path: variant.clone(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let raw = toml::to_string(&settings).unwrap();
            let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
            assert_eq!(parsed.ssh.control_path, variant);
        }
    }

    #[test]
    fn ssh_settings_includes_control_path_default() {
        let settings = ShogunDesktopSettings::default();
        assert_eq!(settings.ssh.control_path, ControlPathType::Socket);
    }

    #[test]
    fn connection_backend_serde_roundtrip() {
        for variant in [ConnectionBackend::System, ConnectionBackend::Native] {
            let settings = ShogunDesktopSettings {
                ssh: SshSettings {
                    connection_backend: variant.clone(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let raw = toml::to_string(&settings).unwrap();
            let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
            assert_eq!(parsed.ssh.connection_backend, variant);
        }
    }

    #[test]
    fn ssh_settings_includes_connection_backend_default() {
        let settings = ShogunDesktopSettings::default();
        assert_eq!(settings.ssh.connection_backend, ConnectionBackend::System);
    }

    #[test]
    fn proxy_command_serde_roundtrip() {
        let settings = ShogunDesktopSettings {
            ssh: SshSettings {
                proxy_command: "coder ssh --stdio %h".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let raw = toml::to_string(&settings).unwrap();
        let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.ssh.proxy_command, "coder ssh --stdio %h");
    }

    #[test]
    fn proxy_command_empty_default() {
        let settings = ShogunDesktopSettings::default();
        assert!(settings.ssh.proxy_command.is_empty());
    }

    #[test]
    fn accept_all_host_keys_serde_roundtrip() {
        let settings = ShogunDesktopSettings {
            ssh: SshSettings {
                accept_all_host_keys: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let raw = toml::to_string(&settings).unwrap();
        let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
        assert!(parsed.ssh.accept_all_host_keys);
    }

    #[test]
    fn accept_all_host_keys_default_false() {
        let settings = ShogunDesktopSettings::default();
        assert!(!settings.ssh.accept_all_host_keys);
    }

    #[test]
    fn terminal_font_serde_roundtrip() {
        let settings = ShogunDesktopSettings {
            terminal: TerminalSettings {
                font: "Cica".into(),
            },
            ..Default::default()
        };
        let raw = toml::to_string(&settings).unwrap();
        let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.terminal.font, "Cica");
    }

    #[test]
    fn terminal_font_default() {
        let settings = ShogunDesktopSettings::default();
        assert_eq!(settings.terminal.font, "Moralerspace Neon HW");
    }

    #[test]
    fn session_settings_default_agents() {
        let settings = ShogunDesktopSettings::default();
        assert_eq!(
            settings.sessions.agents,
            vec![
                "karo".to_string(),
                "gunshi".to_string(),
                "ashigaru1".to_string(),
                "ashigaru2".to_string(),
                "ashigaru3".to_string(),
                "ashigaru4".to_string(),
                "ashigaru5".to_string(),
                "ashigaru6".to_string(),
                "ashigaru7".to_string(),
            ]
        );
    }
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
