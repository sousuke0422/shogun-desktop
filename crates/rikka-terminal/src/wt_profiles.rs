//! Read Windows Terminal's `profiles.list` so RikkaTerminal's new-tab menu
//! can reuse the shells the user already configured, instead of a second
//! copy of that list. Read-only: we never write wt's settings.
//!
//! wt stores only two kinds of profile:
//! - explicit `commandline` (e.g. cmd.exe, custom shells) — used verbatim
//!   after `%VAR%` expansion.
//! - dynamic `source` fragments (PowerShell Core, WSL distros, VS, Azure)
//!   whose command line wt synthesizes at runtime and never writes to disk.
//!   We resolve the ones with a stable command (PowershellCore → pwsh.exe,
//!   WSL/distro → `wsl -d <name>`) and skip the rest (VS dev shells and
//!   Azure need machinery we don't reproduce).

use std::path::PathBuf;

/// A launchable profile distilled from wt's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct WtProfile {
    /// Display name (wt's `name`).
    pub name: String,
    /// Stable id (wt's `guid`), for config on/off and default matching.
    pub guid: String,
    /// Resolved command line: program + args, ready for CommandBuilder.
    pub argv: Vec<String>,
    /// `startingDirectory` if the profile set one.
    pub dir: Option<String>,
    /// wt's `colorScheme` — a scheme name resolved through `wt_schemes`, so a
    /// tab opened from this profile wears that palette (per-tab theming).
    pub color_scheme: Option<String>,
}

/// wt settings.json locations, most canonical first (Store package, then
/// Preview, then unpackaged/scoop installs).
fn settings_candidates() -> Vec<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let mut v = Vec::new();
    if let Some(base) = &local {
        v.push(
            base.join("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json"),
        );
        v.push(base.join(
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        ));
        v.push(base.join("Microsoft/Windows Terminal/settings.json"));
    }
    v
}

/// The wt settings.json the system would use, if any.
pub fn settings_path() -> Option<PathBuf> {
    settings_candidates().into_iter().find(|p| p.exists())
}

/// Discover every launchable wt profile, in wt's own order. `defaultProfile`
/// guid is returned alongside so the caller can honor it. Empty when wt isn't
/// installed or the file can't be parsed.
pub fn discover() -> (Vec<WtProfile>, Option<String>) {
    let Some(path) = settings_path() else {
        return (Vec::new(), None);
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return (Vec::new(), None);
    };
    parse(&raw)
}

/// Parse wt settings JSON text into profiles + the default-profile guid.
/// Separated from IO for testing.
pub fn parse(raw: &str) -> (Vec<WtProfile>, Option<String>) {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(raw)) else {
        return (Vec::new(), None);
    };
    let default = root
        .get("defaultProfile")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let list = root
        .get("profiles")
        .and_then(|p| p.get("list"))
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    // wt applies `profiles.defaults` to every profile; a profile's own
    // `colorScheme` overrides it. Honor the same inheritance for theming.
    let default_scheme = root
        .get("profiles")
        .and_then(|p| p.get("defaults"))
        .and_then(|d| d.get("colorScheme"))
        .and_then(|s| s.as_str())
        .map(str::to_string);

    let mut out = Vec::new();
    for pr in &list {
        // wt hides a profile with "hidden": true — respect it.
        if pr.get("hidden").and_then(|h| h.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(name) = pr.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let guid = pr
            .get("guid")
            .and_then(|g| g.as_str())
            .unwrap_or("")
            .to_string();
        let dir = pr
            .get("startingDirectory")
            .and_then(|d| d.as_str())
            .map(|d| expand_env(d));

        let argv = if let Some(cmd) = pr.get("commandline").and_then(|c| c.as_str()) {
            split_commandline(&expand_env(cmd))
        } else if let Some(src) = pr.get("source").and_then(|s| s.as_str()) {
            match resolve_source(src, name) {
                Some(argv) => argv,
                None => continue, // VS / Azure: not reproducible, skip
            }
        } else {
            continue;
        };
        if argv.is_empty() {
            continue;
        }
        let color_scheme = pr
            .get("colorScheme")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .or_else(|| default_scheme.clone());
        out.push(WtProfile {
            name: name.to_string(),
            guid,
            argv,
            dir,
            color_scheme,
        });
    }
    (out, default)
}

/// Map a dynamic-profile `source` to a command line, or `None` when we can't
/// reproduce it. WSL distros launch via `wsl -d <name>` (the profile name is
/// the distribution name for the WSL/Canonical generators).
fn resolve_source(source: &str, name: &str) -> Option<Vec<String>> {
    if source.eq_ignore_ascii_case("Windows.Terminal.PowershellCore") {
        return Some(vec!["pwsh.exe".to_string()]);
    }
    let wsl = source.contains("WSL")
        || source.contains("Canonical")
        || source.contains("Ubuntu")
        || source.contains("Debian")
        || source.contains("Kali")
        || source.contains("SUSE")
        || source.contains("Linux");
    if wsl {
        return Some(vec![
            "wsl.exe".to_string(),
            "-d".to_string(),
            name.to_string(),
        ]);
    }
    None
}

/// Expand `%NAME%` environment references (wt commandlines use them, e.g.
/// `%SystemRoot%\System32\cmd.exe`). Unknown names are left as written.
fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'%'
            && let Some(end) = s[i + 1..].find('%')
        {
            let name = &s[i + 1..i + 1 + end];
            if !name.is_empty()
                && let Ok(val) = std::env::var(name)
            {
                out.push_str(&val);
                i = i + 1 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Split a command line into argv. Honors double-quoted spans (wt paths with
/// spaces are quoted); everything else splits on whitespace. Good enough for
/// the shells wt profiles actually carry.
fn split_commandline(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(cur);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "defaultProfile": "{574e775e-4f2a-5b96-ac1e-a2962a402336}",
        "profiles": {
            "defaults": {},
            "list": [
                { "name": "Windows PowerShell", "commandline": "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", "hidden": false, "guid": "{61c5}" },
                { "name": "Command Prompt", "commandline": "cmd.exe", "hidden": false, "guid": "{0caa}" },
                { "name": "PowerShell", "source": "Windows.Terminal.PowershellCore", "hidden": false, "guid": "{574e775e-4f2a-5b96-ac1e-a2962a402336}" },
                { "name": "Ubuntu", "source": "CanonicalGroupLimited.Ubuntu_79rhkp1fndgsc", "hidden": false, "guid": "{5185}" },
                { "name": "Azure Cloud Shell", "source": "Windows.Terminal.Azure", "hidden": false, "guid": "{b453}" },
                { "name": "Secret", "commandline": "hush.exe", "hidden": true, "guid": "{dead}" }
            ]
        }
    }"#;

    #[test]
    fn extracts_launchable_profiles_and_default() {
        let (profs, default) = parse(SAMPLE);
        let names: Vec<&str> = profs.iter().map(|p| p.name.as_str()).collect();
        // Azure (unresolvable source) and Secret (hidden) are excluded.
        assert_eq!(
            names,
            [
                "Windows PowerShell",
                "Command Prompt",
                "PowerShell",
                "Ubuntu"
            ]
        );
        assert_eq!(
            default.as_deref(),
            Some("{574e775e-4f2a-5b96-ac1e-a2962a402336}")
        );
    }

    #[test]
    fn color_scheme_reads_per_profile_and_inherits_defaults() {
        let raw = r#"{
            "profiles": {
                "defaults": { "colorScheme": "Campbell" },
                "list": [
                    { "name": "A", "commandline": "a.exe", "colorScheme": "Ubuntu" },
                    { "name": "B", "commandline": "b.exe" }
                ]
            }
        }"#;
        let (profs, _) = parse(raw);
        let by = |n: &str| profs.iter().find(|p| p.name == n).unwrap();
        // Own colorScheme wins; otherwise profiles.defaults is inherited.
        assert_eq!(by("A").color_scheme.as_deref(), Some("Ubuntu"));
        assert_eq!(by("B").color_scheme.as_deref(), Some("Campbell"));
    }

    #[test]
    fn resolves_source_profiles() {
        let (profs, _) = parse(SAMPLE);
        let pwsh = profs.iter().find(|p| p.name == "PowerShell").unwrap();
        assert_eq!(pwsh.argv, ["pwsh.exe"]);
        let ubuntu = profs.iter().find(|p| p.name == "Ubuntu").unwrap();
        assert_eq!(ubuntu.argv, ["wsl.exe", "-d", "Ubuntu"]);
    }

    #[test]
    fn cmd_profile_uses_commandline() {
        let (profs, _) = parse(SAMPLE);
        let cmd = profs.iter().find(|p| p.name == "Command Prompt").unwrap();
        assert_eq!(cmd.argv, ["cmd.exe"]);
    }

    #[test]
    fn strips_comments_and_trailing_commas() {
        let jsonc = r#"{
            // line comment
            "defaultProfile": "{x}", /* block */
            "profiles": { "list": [ { "name": "A", "commandline": "a.exe", "guid": "{a}", }, ] },
        }"#;
        let (profs, default) = parse(jsonc);
        assert_eq!(default.as_deref(), Some("{x}"));
        assert_eq!(profs.len(), 1);
        assert_eq!(profs[0].argv, ["a.exe"]);
    }

    #[test]
    fn split_handles_quoted_paths() {
        assert_eq!(
            split_commandline("\"C:\\Program Files\\x.exe\" -flag arg"),
            ["C:\\Program Files\\x.exe", "-flag", "arg"]
        );
    }

    #[test]
    fn missing_or_broken_settings_is_empty() {
        assert_eq!(parse("not json"), (Vec::new(), None));
        assert_eq!(parse("{}"), (Vec::new(), None));
    }
}

/// Strip JSONC comments and trailing commas so serde_json can parse wt's
/// settings (which allows both). String contents are preserved.
pub(crate) fn strip_jsonc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let b = raw.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    // Drop trailing commas: `,` followed by only whitespace then `}`/`]`.
    let mut cleaned = String::with_capacity(out.len());
    let ob = out.as_bytes();
    let mut j = 0;
    while j < ob.len() {
        if ob[j] == b',' {
            let mut k = j + 1;
            while k < ob.len() && (ob[k] as char).is_whitespace() {
                k += 1;
            }
            if k < ob.len() && (ob[k] == b'}' || ob[k] == b']') {
                j += 1; // skip the comma
                continue;
            }
        }
        cleaned.push(ob[j] as char);
        j += 1;
    }
    cleaned
}
