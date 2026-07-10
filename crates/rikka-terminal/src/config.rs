//! RikkaTerminal's own config (`%APPDATA%/rikka-terminal/config.toml`), whose
//! job today is turning Windows Terminal profiles on/off in the new-tab menu.
//! The wt list is the source of truth; this file is a thin filter over it, so
//! a user who lives in wt gets their shells for free and can hide the ones
//! they never launch here. Own profiles are defined inline and lead the menu.
//!
//! ```toml
//! [profiles]
//! use_windows_terminal = true          # pull wt's profiles into the menu
//! hidden = ["Azure Cloud Shell", "{guid}"]  # drop these (name or guid)
//! default = "Dev"                      # new-tab default (name or guid);
//!                                       # omitted = wt's own defaultProfile
//!
//! [[profiles.custom]]                  # own profiles (lead the list)
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
    /// Profiles defined here in rikka's own config; they lead the menu.
    #[serde(default)]
    pub custom: Vec<CustomProfile>,
}

/// A profile defined in rikka's config rather than borrowed from wt.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomProfile {
    /// Display name and tab title.
    pub name: String,
    /// Command line as argv; `command[0]` is the program to spawn.
    pub command: Vec<String>,
    /// Optional starting directory.
    #[serde(default)]
    pub dir: Option<String>,
}

impl CustomProfile {
    /// A synthetic guid (`custom:<name>`) keeps config `default`/`hidden`
    /// matching uniform across wt and own profiles.
    fn to_profile(&self) -> WtProfile {
        WtProfile {
            name: self.name.clone(),
            guid: format!("custom:{}", self.name),
            argv: self.command.clone(),
            dir: self.dir.clone(),
        }
    }
}

impl Default for ProfilesSection {
    fn default() -> Self {
        Self {
            use_windows_terminal: true,
            hidden: Vec::new(),
            default: None,
            custom: Vec::new(),
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
        // Own profiles lead the list; wt's follow when enabled. A custom
        // profile with an empty command line is ignored.
        let mut all: Vec<WtProfile> = self
            .profiles
            .custom
            .iter()
            .filter(|c| !c.command.is_empty())
            .map(CustomProfile::to_profile)
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
            profiles: ProfilesSection {
                use_windows_terminal: true,
                hidden: vec!["Ubuntu".into(), "{c}".into()],
                default: None,
                custom: vec![],
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
            profiles: ProfilesSection {
                use_windows_terminal: true,
                hidden: vec![],
                default: Some("Ubuntu".into()),
                custom: vec![],
            },
        };
        let m = cfg.build_menu(wt(), Some("{p}".into()));
        assert_eq!(m.default, Some(2));
    }

    #[test]
    fn opt_out_yields_empty_menu() {
        let cfg = Config {
            profiles: ProfilesSection {
                use_windows_terminal: false,
                ..Default::default()
            },
        };
        assert_eq!(cfg.build_menu(wt(), None), Menu::default());
    }

    #[test]
    fn custom_profiles_lead_the_menu() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             default = \"Dev\"\n\
             [[profiles.custom]]\n\
             name = \"Dev\"\n\
             command = [\"pwsh.exe\", \"-NoLogo\"]\n\
             dir = \"C:/work\"\n",
        )
        .unwrap();
        let m = cfg.build_menu(wt(), Some("{p}".into()));
        assert_eq!(m.profiles.len(), 4); // 1 custom + 3 wt
        assert_eq!(m.profiles[0].name, "Dev");
        assert_eq!(m.profiles[0].argv, ["pwsh.exe", "-NoLogo"]);
        assert_eq!(m.profiles[0].dir.as_deref(), Some("C:/work"));
        assert_eq!(m.default, Some(0)); // Dev by name, over wt's default
    }

    #[test]
    fn custom_only_with_wt_disabled() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             use_windows_terminal = false\n\
             [[profiles.custom]]\n\
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
    fn empty_custom_command_is_ignored() {
        let cfg: Config = toml::from_str(
            "[profiles]\n\
             use_windows_terminal = false\n\
             [[profiles.custom]]\n\
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
