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
    /// OSC 9 / OSC 777 desktop notifications (equivalent of Ghostty's
    /// `desktop-notifications`).
    #[serde(default = "default_true")]
    pub desktop_notifications: bool,
    /// Notifications from the 家老陣 (multiagent) tab. Off by default — that
    /// tmux session hosts many agents and would toast constantly. ANDed with
    /// `desktop_notifications`.
    #[serde(default)]
    pub desktop_notifications_multiagent: bool,
    /// What XTVERSION (`CSI > 0 q`) claims this terminal is. `Honest` names
    /// the engine (rikka-terminal); `Ghostty` masquerades so emulator-sniffing apps
    /// (e.g. yazi picks its kitty-graphics adapter by known terminal names)
    /// enable their good paths. We implement the capabilities Ghostty
    /// advertises this way: kitty graphics, kitty keyboard, OSC 8/9/52, sixel.
    #[serde(default)]
    pub identity: TerminalIdentity,
    /// TERM name sent in the SSH pty-req (and TERM env on the ssh.exe
    /// path). This is a *terminfo lookup key* on the remote, not a greeting:
    /// pick a name the remote actually has an entry for, or full-screen
    /// apps (vim/less/tmux) break. See the settings-tab warnings.
    #[serde(default)]
    pub term: TermName,
    /// OpenType features for terminal text, comma/space separated 4-char
    /// tags: `ss01, ss03` (on), `calt=0` (explicit off), `cv02=3`
    /// (alternate). Monaspace/Moralerspace keeps its ligatures in stylistic
    /// sets: ss01 ==/!=, ss02 >=/<=, ss03 arrows, ss04 </ />, ss05 |>,
    /// ss07 ::, ss08 .= — enable per feature to taste.
    #[serde(default)]
    pub font_features: String,
    /// Ask the remote tmux to forward pane titles (`set-titles on`), so the
    /// terminal can turn an agent's title spinner into a progress bar when the
    /// agent can't use OSC 9;4 (Claude Code inside tmux). On by default; turning
    /// it off stops shogun-desktop from touching the remote's global tmux
    /// title settings. Implemented by injecting `RIKKA_TMUX_TITLES=1` — the
    /// remote rc/attach routes act only when that var is set.
    #[serde(default = "default_true")]
    pub tmux_forward_titles: bool,
    /// Windows TSF IME path. On: give the taskbar input indicator (あ/A) a
    /// real text-store-backed focus document so it tracks this window, and
    /// route composition through TSF (Google 日本語入力 / Mozc then compose
    /// correctly). Off: fall back to the plain IMM32 path (no indicator
    /// tracking). On by default. The `SHOGUN_TSF` env var, if set, overrides
    /// this either way (`SHOGUN_TSF=0` forces off) — used by the e2e harness.
    #[serde(default = "default_true")]
    pub tsf: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TermName {
    /// Present on every machine; capabilities match what we implement.
    #[default]
    Xterm256color,
    /// Requires Ghostty's terminfo on the remote (`infocmp xterm-ghostty`).
    XtermGhostty,
    /// RikkaTerminal's own terminfo — not distributed yet; almost certainly
    /// missing on remotes. Future: kitten-ssh-style auto-install.
    XtermRikka,
}

impl TermName {
    pub fn as_str(self) -> &'static str {
        match self {
            TermName::Xterm256color => "xterm-256color",
            TermName::XtermGhostty => "xterm-ghostty",
            TermName::XtermRikka => "xterm-rikka",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TerminalIdentity {
    #[default]
    Honest,
    Ghostty,
}

impl TerminalIdentity {
    /// The XTVERSION reply body (`DCS >| <this> ST`).
    pub fn xtversion(self) -> String {
        match self {
            TerminalIdentity::Honest => rikka_terminal_core::xtversion::engine_identity(),
            // Current Ghostty release; apps key off the name.
            TerminalIdentity::Ghostty => "ghostty 1.3.1".to_string(),
        }
    }

    /// Returns `(TERM_PROGRAM, TERM_PROGRAM_VERSION)` env vars to inject.
    pub fn term_program_env(self) -> (&'static str, &'static str) {
        match self {
            TerminalIdentity::Honest => (
                rikka_terminal_core::xtversion::TERM_PROGRAM,
                rikka_terminal_core::xtversion::TERM_PROGRAM_VERSION,
            ),
            TerminalIdentity::Ghostty => ("ghostty", "1.3.1"),
        }
    }
}

/// Parse the free-form feature string into (tag, value) pairs, dropping
/// anything that isn't a 4-character OpenType tag.
pub fn parse_font_features(raw: &str) -> Vec<(String, u32)> {
    raw.split([',', ' ', '\t'])
        .filter(|t| !t.is_empty())
        .filter_map(|item| {
            let (tag, value) = match item.split_once('=') {
                Some((tag, v)) => (tag.trim(), v.trim().parse().ok()?),
                None => (item.trim(), 1),
            };
            (tag.len() == 4 && tag.chars().all(|c| c.is_ascii_alphanumeric()))
                .then(|| (tag.to_string(), value))
        })
        .collect()
}

fn default_terminal_font() -> String {
    "Moralerspace Neon HW".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font: default_terminal_font(),
            desktop_notifications: true,
            desktop_notifications_multiagent: false,
            identity: TerminalIdentity::default(),
            term: TermName::default(),
            font_features: String::new(),
            tmux_forward_titles: true,
            tsf: true,
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
    fn terminal_identity_serde_roundtrip_and_default() {
        // Default = honest; masquerade round-trips through TOML.
        assert_eq!(
            ShogunDesktopSettings::default().terminal.identity,
            TerminalIdentity::Honest
        );
        let settings = ShogunDesktopSettings {
            terminal: TerminalSettings {
                identity: TerminalIdentity::Ghostty,
                ..Default::default()
            },
            ..Default::default()
        };
        let raw = toml::to_string(&settings).unwrap();
        let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.terminal.identity, TerminalIdentity::Ghostty);
        assert_eq!(parsed.terminal.identity.xtversion(), "ghostty 1.3.1");
        assert!(
            TerminalIdentity::Honest
                .xtversion()
                .starts_with("rikka-terminal ")
        );
    }

    #[test]
    fn font_features_parser_accepts_tags_and_values() {
        assert_eq!(
            parse_font_features("ss01, ss03 calt=0\tcv02=3"),
            vec![
                ("ss01".to_string(), 1),
                ("ss03".to_string(), 1),
                ("calt".to_string(), 0),
                ("cv02".to_string(), 3),
            ]
        );
        // Garbage and non-4-char tags are dropped; empty input is empty.
        assert!(parse_font_features("").is_empty());
        assert!(parse_font_features("liga!, ss, =1, ss001").is_empty());
    }

    #[test]
    fn term_name_serde_roundtrip_and_default() {
        assert_eq!(
            ShogunDesktopSettings::default().terminal.term,
            TermName::Xterm256color
        );
        assert_eq!(TermName::Xterm256color.as_str(), "xterm-256color");
        for variant in [TermName::XtermGhostty, TermName::XtermRikka] {
            let settings = ShogunDesktopSettings {
                terminal: TerminalSettings {
                    term: variant,
                    ..Default::default()
                },
                ..Default::default()
            };
            let raw = toml::to_string(&settings).unwrap();
            let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
            assert_eq!(parsed.terminal.term, variant);
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
                desktop_notifications: false,
                desktop_notifications_multiagent: true,
                identity: TerminalIdentity::Ghostty,
                term: TermName::XtermGhostty,
                font_features: "ss01, ss03".into(),
                tmux_forward_titles: false,
                tsf: false,
            },
            ..Default::default()
        };
        let raw = toml::to_string(&settings).unwrap();
        let parsed: ShogunDesktopSettings = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.terminal.font, "Cica");
        assert!(!parsed.terminal.desktop_notifications);
        assert!(parsed.terminal.desktop_notifications_multiagent);
        assert!(!parsed.terminal.tsf);
    }

    #[test]
    fn desktop_notifications_defaults_when_absent() {
        let parsed: ShogunDesktopSettings = toml::from_str("[terminal]\nfont = \"x\"\n").unwrap();
        // Master switch on; the noisy multiagent tab swallowed by default.
        assert!(parsed.terminal.desktop_notifications);
        assert!(!parsed.terminal.desktop_notifications_multiagent);
    }

    #[test]
    fn tsf_defaults_on_when_absent() {
        // Existing settings files predate the field; it must default on so the
        // TSF IME path is the out-of-the-box behavior.
        let parsed: ShogunDesktopSettings = toml::from_str("[terminal]\nfont = \"x\"\n").unwrap();
        assert!(parsed.terminal.tsf);
        assert!(ShogunDesktopSettings::default().terminal.tsf);
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
