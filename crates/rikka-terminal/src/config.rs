//! RikkaTerminal's own config (`%APPDATA%/rikka-terminal/config.toml`), whose
//! job today is turning Windows Terminal profiles on/off in the new-tab menu.
//! The wt list is the source of truth; this file is a thin filter over it, so
//! a user who lives in wt gets their shells for free and can hide the ones
//! they never launch here. Own profiles are first-class (`[[profiles.list]]`,
//! works on any platform) and lead the menu.
//!
//! ```toml
//! [profiles]
//! use_windows_terminal = true          # pull wt's profiles into the menu
//! hidden = ["Azure Cloud Shell", "{guid}"]  # drop these (name or guid)
//! default = "Dev"                      # new-tab default (name or guid);
//!                                       # omitted = wt's own defaultProfile
//!
//! [[profiles.list]]                    # own profiles (any platform; lead the menu)
//! name = "Dev"
//! command = ["pwsh.exe", "-NoLogo"]    # argv; command[0] is the program
//! dir = "C:\\work"                     # optional starting directory
//! ```

use serde::Deserialize;

use crate::wt_profiles::WtProfile;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub profiles: ProfilesSection,
    #[serde(default)]
    pub appearance: AppearanceSection,
    #[serde(default)]
    pub terminal: TerminalSection,
    #[serde(default)]
    pub logging: LoggingSection,
    #[serde(default)]
    pub keys: KeysSection,
    #[serde(default)]
    pub theme: ThemeSection,
}

/// ```toml
/// [appearance]
/// font = "Cascadia Mono"   # grid font (default: Consolas)
/// font_size = 14.0         # logical px (default: 13.0)
/// line_height = 1.3        # cell height = font_size × this (default: 1.2)
/// acrylic = true           # blurred window background (default: off;
///                          # the RIKKA_ACRYLIC env var still works)
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppearanceSection {
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default)]
    pub font_size: Option<f32>,
    #[serde(default)]
    pub line_height: Option<f32>,
    #[serde(default)]
    pub acrylic: Option<bool>,
}

/// ```toml
/// [terminal]
/// scrollback = 50000       # history lines per tab (default: 10000)
/// term = "xterm-256color"  # TERM for spawned shells (default: xterm-256color;
///                          # what konsole/alacritty set — cross-platform TUIs
///                          # like btop read it for color/capability detection)
/// identity = "honest"      # XTVERSION + TERM_PROGRAM: "honest" (rikka-terminal)
///                          # or "ghostty" (masquerade so emulator-sniffing apps
///                          # enable kitty features). Default honest — a spoof
///                          # over ConPTY invites conhost-stripped features.
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TerminalSection {
    #[serde(default)]
    pub scrollback: Option<u32>,
    #[serde(default)]
    pub term: Option<String>,
    #[serde(default)]
    pub identity: Option<String>,
}

/// Tera Term-style session logging (Ctrl+Shift+L per tab; see session_log).
///
/// ```toml
/// [logging]
/// directory = 'C:\logs'    # save dir (default: ~/Documents/rikka-terminal-logs)
/// log_input = true         # ALSO record keystrokes into *.input.log
///                          # (default off — an input log captures typed
///                          # passwords; opt in deliberately)
/// auto_start = true        # every new tab starts logging (default off)
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggingSection {
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default)]
    pub log_input: Option<bool>,
    #[serde(default)]
    pub auto_start: Option<bool>,
}

/// Chord reassignment for the tab-management keys (parser and defaults in
/// `keymap.rs`; unset = the built-in Ctrl+Shift chord).
///
/// ```toml
/// [keys]
/// new_tab = "ctrl+shift+t"     # value shape: "mod+mod+key",
/// close_tab = "ctrl+shift+w"   # mods: ctrl / shift / alt
/// detach_tab = "ctrl+shift+d"
/// eject_tab = "ctrl+shift+e"
/// move_tab = "ctrl+shift+x"
/// merge_all = "ctrl+shift+a"
/// toggle_logging = "ctrl+shift+l"
/// copy = "ctrl+shift+c"
/// paste = "ctrl+shift+v"
/// cycle_back = "ctrl+shift+tab"
/// ```
/// Terminal color palette (applied in `keymap`-style at startup; the resolver
/// lives in `wt_schemes` + engine `theme`).
///
/// ```toml
/// [theme]
/// wt_scheme = "Ubuntu"       # import a Windows Terminal scheme BY NAME
///                            # (from wt's settings.json + fragment dirs)
/// # inline overrides win over the imported scheme; each is "#RRGGBB":
/// background = "#300A24"
/// foreground = "#EEEEEC"
/// selection = "#B5D5FF"
/// # ansi = [16 × "#RRGGBB"]  # black..white, brightBlack..brightWhite
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ThemeSection {
    /// A Windows Terminal color scheme to import by name (compat mode).
    #[serde(default)]
    pub wt_scheme: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub selection: Option<String>,
    /// The 16 ANSI colors; when present must hold exactly 16 `#RRGGBB`
    /// entries (black..white, brightBlack..brightWhite).
    #[serde(default)]
    pub ansi: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct KeysSection {
    #[serde(default)]
    pub new_tab: Option<String>,
    #[serde(default)]
    pub close_tab: Option<String>,
    #[serde(default)]
    pub detach_tab: Option<String>,
    #[serde(default)]
    pub eject_tab: Option<String>,
    #[serde(default)]
    pub move_tab: Option<String>,
    #[serde(default)]
    pub merge_all: Option<String>,
    #[serde(default)]
    pub toggle_logging: Option<String>,
    #[serde(default)]
    pub copy: Option<String>,
    #[serde(default)]
    pub paste: Option<String>,
    #[serde(default)]
    pub cycle_back: Option<String>,
    #[serde(default)]
    pub jump_prompt_prev: Option<String>,
    #[serde(default)]
    pub jump_prompt_next: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfilesSection {
    /// Pull wt's profiles into the new-tab menu (default true).
    #[serde(default = "default_true")]
    pub use_windows_terminal: bool,
    /// Profiles to drop, by name or guid.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// New-tab default, by name or guid. `None` = honor wt's defaultProfile.
    #[serde(default)]
    pub default: Option<String>,
    /// Profiles defined in rikka's own config; platform-neutral and first —
    /// they lead the menu ahead of any imported wt profiles.
    #[serde(default)]
    pub list: Vec<ProfileDef>,
}

/// A profile defined in rikka's config rather than borrowed from wt.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileDef {
    /// Display name and tab title.
    pub name: String,
    /// Command line as argv; `command[0]` is the program to spawn.
    pub command: Vec<String>,
    /// Optional starting directory.
    #[serde(default)]
    pub dir: Option<String>,
    /// Windows Terminal color-scheme name for tabs opened from this profile
    /// (resolved through `wt_schemes`) — per-profile theming.
    #[serde(default)]
    pub theme: Option<String>,
}

impl ProfileDef {
    /// A synthetic guid (`user:<name>`) keeps config `default`/`hidden`
    /// matching uniform across wt and own profiles.
    fn to_profile(&self) -> WtProfile {
        WtProfile {
            name: self.name.clone(),
            guid: format!("user:{}", self.name),
            argv: self.command.clone(),
            dir: self.dir.clone(),
            color_scheme: self.theme.clone(),
        }
    }
}

impl Default for ProfilesSection {
    fn default() -> Self {
        Self {
            use_windows_terminal: true,
            hidden: Vec::new(),
            default: None,
            list: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// `%APPDATA%/rikka-terminal/config.toml`.
pub fn config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA")
        .map(|base| std::path::PathBuf::from(base).join("rikka-terminal/config.toml"))
}

impl Config {
    /// Load the config, or defaults when it is absent or unparsable (a broken
    /// config must never stop the terminal from opening).
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&raw).unwrap_or_default()
    }
}

/// The new-tab menu after applying the config to wt's raw profile list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Menu {
    pub profiles: Vec<WtProfile>,
    /// Index into `profiles` opened by a plain new-tab; `None` when empty.
    pub default: Option<usize>,
}

impl Config {
    /// Filter/order wt's profiles into the menu: honor `use_windows_terminal`,
    /// drop `hidden` (by name or guid), and pick the default (config first,
    /// else wt's defaultProfile guid, else the first profile).
    pub fn build_menu(&self, wt: Vec<WtProfile>, wt_default_guid: Option<String>) -> Menu {
        // Own profiles lead the list; wt's follow when enabled. An own
        // profile with an empty command line is ignored.
        let mut all: Vec<WtProfile> = self
            .profiles
            .list
            .iter()
            .filter(|c| !c.command.is_empty())
            .map(ProfileDef::to_profile)
            .collect();
        if self.profiles.use_windows_terminal {
            all.extend(wt);
        }
        let hidden = &self.profiles.hidden;
        let profiles: Vec<WtProfile> = all
            .into_iter()
            .filter(|p| !hidden.iter().any(|h| h == &p.name || h == &p.guid))
            .collect();
        if profiles.is_empty() {
            return Menu::default();
        }
        let default = self
            .profiles
            .default
            .as_ref()
            .and_then(|d| profiles.iter().position(|p| &p.name == d || &p.guid == d))
            .or_else(|| {
                wt_default_guid
                    .as_ref()
                    .and_then(|g| profiles.iter().position(|p| &p.guid == g))
            })
            .or(Some(0));
        Menu { profiles, default }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prof(name: &str, guid: &str) -> WtProfile {
        WtProfile {
            name: name.into(),
            guid: guid.into(),
            argv: vec![format!("{name}.exe")],
            dir: None,
            color_scheme: None,
        }
    }

    fn wt() -> Vec<WtProfile> {
        vec![
            prof("cmd", "{c}"),
            prof("PowerShell", "{p}"),
            prof("Ubuntu", "{u}"),
        ]
    }

    #[test]
    fn default_config_keeps_all_and_honors_wt_default() {
        let m = Config::default().build_menu(wt(), Some("{p}".into()));
        assert_eq!(m.profiles.len(), 3);
        assert_eq!(m.default, Some(1)); // PowerShell
    }

    #[test]
    fn hidden_drops_by_name_or_guid() {
        let cfg = Config {
            appearance: AppearanceSection::default(),
            terminal: TerminalSection::default(),
            logging: LoggingSection::default(),
            keys: KeysSection::default(),
            theme: ThemeSection::default(),
            profiles: ProfilesSection {
                use_windows_terminal: true,
                hidden: vec!["Ubuntu".into(), "{c}".into()],
                default: None,
                list: vec![],
            },
        };
        let m = cfg.build_menu(wt(), Some("{p}".into()));
        let names: Vec<&str> = m.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["PowerShell"]);
        assert_eq!(m.default, Some(0));
    }

    #[test]
    fn explicit_default_wins_over_wt() {
        let cfg = Config {
            appearance: AppearanceSection::default(),
            terminal: TerminalSection::default(),
            logging: LoggingSection::default(),
            keys: KeysSection::default(),
            theme: ThemeSection::default(),
            profiles: ProfilesSection {
                use_windows_terminal: true,
                hidden: vec![],
                default: Some("Ubuntu".into()),
                list: vec![],
            },
        };
        let m = cfg.build_menu(wt(), Some("{p}".into()));
        assert_eq!(m.default, Some(2));
    }

    #[test]
    fn opt_out_yields_empty_menu() {
        let cfg = Config {
            appearance: AppearanceSection::default(),
            terminal: TerminalSection::default(),
            logging: LoggingSection::default(),
            keys: KeysSection::default(),
            theme: ThemeSection::default(),
            profiles: ProfilesSection {
                use_windows_terminal: false,
                ..Default::default()
            },
        };
        assert_eq!(cfg.build_menu(wt(), None), Menu::default());
    }

    #[test]
    fn list_profiles_lead_the_menu() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             default = \"Dev\"\n\
             [[profiles.list]]\n\
             name = \"Dev\"\n\
             command = [\"pwsh.exe\", \"-NoLogo\"]\n\
             dir = \"C:/work\"\n",
        )
        .unwrap();
        let m = cfg.build_menu(wt(), Some("{p}".into()));
        assert_eq!(m.profiles.len(), 4); // 1 own + 3 wt
        assert_eq!(m.profiles[0].name, "Dev");
        assert_eq!(m.profiles[0].argv, ["pwsh.exe", "-NoLogo"]);
        assert_eq!(m.profiles[0].dir.as_deref(), Some("C:/work"));
        assert_eq!(m.default, Some(0)); // Dev by name, over wt's default
    }

    #[test]
    fn list_only_with_wt_disabled() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             use_windows_terminal = false\n\
             [[profiles.list]]\n\
             name = \"Bash\"\n\
             command = [\"bash.exe\"]\n",
        )
        .unwrap();
        let m = cfg.build_menu(wt(), None);
        assert_eq!(m.profiles.len(), 1);
        assert_eq!(m.profiles[0].name, "Bash");
        assert_eq!(m.default, Some(0));
    }

    #[test]
    fn empty_list_command_is_ignored() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             use_windows_terminal = false\n\
             [[profiles.list]]\n\
             name = \"Broken\"\n\
             command = []\n",
        )
        .unwrap();
        assert_eq!(cfg.build_menu(wt(), None), Menu::default());
    }

    #[test]
    fn toml_round_trip() {
        let raw = r#"
            [profiles]
            use_windows_terminal = true
            hidden = ["Azure Cloud Shell"]
            default = "PowerShell"
        "#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.profiles.hidden, ["Azure Cloud Shell"]);
        assert_eq!(cfg.profiles.default.as_deref(), Some("PowerShell"));
    }
}
