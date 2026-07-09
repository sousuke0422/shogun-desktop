//! wt-compatible command line (`rt` = RikkaTerminal).
//!
//! Grammar mirrors Windows Terminal's `wt.exe`:
//!
//! ```text
//! rt [global-options] [subcommand [options] [commandline...]] [; subcommand ...]
//! ```
//!
//! Supported today:
//! - globals: `-M/--maximized`, `-F/--fullscreen`, `--pos x,y`, `--size c,r`
//!   (cells), `-f/--focus` (accepted, no-op — we always take focus)
//! - `new-tab` / `nt` (and the implicit form: bare positionals run as the
//!   pane's command): `-p/--profile` (shell name), `-d/--startingDirectory`,
//!   `--title`, plus accepted-and-ignored cosmetics (`--tabColor`,
//!   `--colorScheme`, `--suppressApplicationTitle`, `--useApplicationTitle`)
//! - `;` between commands: every `new-tab` lands in the ONE window this
//!   process opens, same as a fresh `wt` launch
//!
//! Deliberately TODO (structural work, tracked in README):
//! - `-w/--window` routing to an existing window (needs single-instance IPC)
//! - `split-pane`/`sp` (needs pane splits), `focus-tab`, `move-focus`,
//!   `move-pane`, `focus-pane`
//!
//! Errors surface via [`error_box`] — the release binary is GUI-subsystem,
//! so there is no console to print usage to.

/// One pane-to-be (today: one tab in the launch window).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TabSpec {
    /// `-p/--profile`: mapped to the shell executable to launch.
    pub profile: Option<String>,
    /// `-d/--startingDirectory`.
    pub dir: Option<String>,
    /// `--title`: initial tab title (OSC 0/2 may later overwrite it).
    pub title: Option<String>,
    /// Positional command line (argv, already OS-split). Replaces the shell.
    pub cmdline: Vec<String>,
}

/// Parsed launch request.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Launch {
    pub maximized: bool,
    pub fullscreen: bool,
    /// `--pos x,y` in screen pixels.
    pub pos: Option<(f32, f32)>,
    /// `--size c,r` in CELLS (wt semantics).
    pub size_cells: Option<(u16, u16)>,
    /// One entry per `new-tab` command; empty = single default tab.
    pub tabs: Vec<TabSpec>,
}

fn value_of(
    it: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    opt: &str,
) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{opt} に値がありません"))
}

fn parse_pair(s: &str, opt: &str) -> Result<(f32, f32), String> {
    let mut parts = s.split(',');
    let a = parts.next().and_then(|v| v.trim().parse::<f32>().ok());
    let b = parts.next().and_then(|v| v.trim().parse::<f32>().ok());
    match (a, b, parts.next()) {
        (Some(a), Some(b), None) => Ok((a, b)),
        _ => Err(format!("{opt} は \"x,y\" 形式で指定してください: {s}")),
    }
}

/// Parse `rt` arguments (argv without the program name).
pub fn parse(args: Vec<String>) -> Result<Launch, String> {
    let mut launch = Launch::default();

    // Split into `;`-delimited command groups (wt separates commands with a
    // standalone `;` token; `\;` escapes a literal semicolon positional).
    let mut groups: Vec<Vec<String>> = vec![Vec::new()];
    for a in args {
        if a == ";" {
            groups.push(Vec::new());
        } else if a == "\\;" {
            groups.last_mut().unwrap().push(";".into());
        } else {
            groups.last_mut().unwrap().push(a);
        }
    }

    for (gi, group) in groups.into_iter().enumerate() {
        let mut it = group.into_iter().peekable();

        // Global options may only lead the FIRST command group.
        if gi == 0 {
            while let Some(a) = it.peek() {
                match a.as_str() {
                    "-M" | "--maximized" => {
                        launch.maximized = true;
                        it.next();
                    }
                    "-F" | "--fullscreen" => {
                        launch.fullscreen = true;
                        it.next();
                    }
                    "-f" | "--focus" => {
                        it.next(); // always focused; accepted for compat
                    }
                    "--pos" => {
                        it.next();
                        let v = value_of(&mut it, "--pos")?;
                        launch.pos = Some(parse_pair(&v, "--pos")?);
                    }
                    "--size" => {
                        it.next();
                        let v = value_of(&mut it, "--size")?;
                        let (c, r) = parse_pair(&v, "--size")?;
                        launch.size_cells = Some((c as u16, r as u16));
                    }
                    "-w" | "--window" => {
                        return Err("-w/--window (既存ウィンドウへのルーティング) は未対応です \
                             (TODO: 単一インスタンス IPC)"
                            .into());
                    }
                    "-h" | "--help" | "-?" | "/?" => {
                        return Err(HELP.into());
                    }
                    _ => break,
                }
            }
        }

        // Subcommand (implicit new-tab when the first token isn't one).
        let sub = match it.peek().map(|s| s.as_str()) {
            None => {
                // Empty group: `rt` alone (or a trailing `;`) = default tab,
                // but only materialize it for the first group.
                if gi == 0 {
                    continue;
                } else {
                    launch.tabs.push(TabSpec::default());
                    continue;
                }
            }
            Some("new-tab") | Some("nt") => {
                it.next();
                "new-tab"
            }
            Some("split-pane") | Some("sp") => {
                return Err("split-pane は未対応です (TODO: ペイン分割)".into());
            }
            Some("focus-tab") | Some("ft") | Some("move-focus") | Some("mf")
            | Some("move-pane") | Some("mp") | Some("focus-pane") | Some("fp") => {
                return Err(format!(
                    "{} は未対応です (TODO)",
                    it.peek().map(|s| s.as_str()).unwrap_or_default()
                ));
            }
            // Bare positionals / options → implicit new-tab (wt behavior:
            // `wt ping example.com` runs ping in the new tab).
            Some(_) => "new-tab",
        };
        debug_assert_eq!(sub, "new-tab");

        let mut spec = TabSpec::default();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" | "--profile" => spec.profile = Some(value_of(&mut it, "-p")?),
                "-d" | "--startingDirectory" => spec.dir = Some(value_of(&mut it, "-d")?),
                "--title" => spec.title = Some(value_of(&mut it, "--title")?),
                // Accepted for wt compatibility; no visual counterpart yet.
                "--tabColor" | "--colorScheme" => {
                    let _ = value_of(&mut it, &a)?;
                }
                "--suppressApplicationTitle" | "--useApplicationTitle" => {}
                _ if a.starts_with('-') && spec.cmdline.is_empty() => {
                    return Err(format!("不明なオプション: {a}"));
                }
                // First non-option token starts the command line; everything
                // after belongs to it verbatim (options included).
                _ => {
                    spec.cmdline.push(a);
                    spec.cmdline.extend(it.by_ref());
                }
            }
        }
        launch.tabs.push(spec);
    }

    Ok(launch)
}

/// `code`-style directory positionals (an rt extension over wt): when a
/// tab's command line is nothing but existing directories, it wasn't a
/// command — open the default shell there instead, one tab per directory.
/// Executing a directory could never succeed, so no wt behavior is lost.
/// An explicit `-d` wins (the positionals stay a command line then).
pub fn expand_dir_tabs(tabs: Vec<TabSpec>) -> Vec<TabSpec> {
    tabs.into_iter()
        .flat_map(|spec| {
            let all_dirs = !spec.cmdline.is_empty()
                && spec.dir.is_none()
                && spec
                    .cmdline
                    .iter()
                    .all(|tok| std::path::Path::new(tok).is_dir());
            if !all_dirs {
                return vec![spec];
            }
            spec.cmdline
                .iter()
                .map(|dir| TabSpec {
                    profile: spec.profile.clone(),
                    dir: Some(dir.clone()),
                    title: spec.title.clone(),
                    cmdline: Vec::new(),
                })
                .collect()
        })
        .collect()
}

pub const HELP: &str = "rt — RikkaTerminal (wt 互換 CLI)\n\n\
rt [options] [new-tab [tab-options] [command...]] [; new-tab ...]\n\
rt <dir> [...]  — code コマンド流: そのディレクトリでシェルを開く (1個1タブ)\n\n\
options:\n  -M, --maximized      最大化で起動\n  -F, --fullscreen     フルスクリーンで起動\n  \
--pos x,y            ウィンドウ位置 (px)\n  --size c,r           サイズ (セル数)\n\n\
new-tab (nt):\n  -p, --profile <shell>          シェル (pwsh / powershell / cmd / 実行ファイル)\n  \
-d, --startingDirectory <dir>  開始ディレクトリ\n  --title <title>                初期タブタイトル\n  \
<command...>                   シェルの代わりに実行するコマンド\n\n\
未対応 (TODO): -w/--window, split-pane, focus-tab, move-focus, move-pane";

/// Modal error/usage box — the GUI-subsystem binary has no console.
#[cfg(windows)]
pub fn error_box(msg: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from("rt (RikkaTerminal)"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
pub fn error_box(msg: &str) {
    eprintln!("{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_launch_is_one_default_tab() {
        let l = parse(v(&[])).unwrap();
        assert_eq!(l, Launch::default());
    }

    #[test]
    fn wt_style_globals() {
        let l = parse(v(&["-M", "--pos", "100,200", "--size", "120,40"])).unwrap();
        assert!(l.maximized);
        assert_eq!(l.pos, Some((100.0, 200.0)));
        assert_eq!(l.size_cells, Some((120, 40)));
    }

    #[test]
    fn new_tab_options_and_alias() {
        let l = parse(v(&["nt", "-d", "C:\\work", "--title", "作業", "-p", "cmd"])).unwrap();
        assert_eq!(l.tabs.len(), 1);
        let t = &l.tabs[0];
        assert_eq!(t.dir.as_deref(), Some("C:\\work"));
        assert_eq!(t.title.as_deref(), Some("作業"));
        assert_eq!(t.profile.as_deref(), Some("cmd"));
    }

    #[test]
    fn implicit_new_tab_runs_commandline() {
        // `rt ping -n 3 localhost` — options after the program belong to it.
        let l = parse(v(&["ping", "-n", "3", "localhost"])).unwrap();
        assert_eq!(l.tabs[0].cmdline, v(&["ping", "-n", "3", "localhost"]));
    }

    #[test]
    fn semicolon_chains_tabs_in_one_window() {
        let l = parse(v(&[
            "new-tab", "-d", "C:\\", ";", "nt", "-p", "pwsh", ";", "htop",
        ]))
        .unwrap();
        assert_eq!(l.tabs.len(), 3);
        assert_eq!(l.tabs[0].dir.as_deref(), Some("C:\\"));
        assert_eq!(l.tabs[1].profile.as_deref(), Some("pwsh"));
        assert_eq!(l.tabs[2].cmdline, v(&["htop"]));
    }

    #[test]
    fn escaped_semicolon_is_a_literal() {
        let l = parse(v(&["cmd", "/c", "echo", "\\;"])).unwrap();
        assert_eq!(l.tabs[0].cmdline, v(&["cmd", "/c", "echo", ";"]));
    }

    #[test]
    fn unsupported_subcommands_error_as_todo() {
        assert!(parse(v(&["split-pane"])).unwrap_err().contains("TODO"));
        assert!(parse(v(&["-w", "0", "nt"])).unwrap_err().contains("TODO"));
    }

    #[test]
    fn cosmetics_are_accepted_and_ignored() {
        let l = parse(v(&[
            "nt",
            "--tabColor",
            "#ff0000",
            "--suppressApplicationTitle",
            "--colorScheme",
            "Campbell",
        ]))
        .unwrap();
        assert_eq!(l.tabs[0], TabSpec::default());
    }

    #[test]
    fn bare_directory_positional_becomes_cwd() {
        let tmp = std::env::temp_dir();
        let t = tmp.to_string_lossy().to_string();
        let l = parse(v(&[&t])).unwrap();
        let tabs = expand_dir_tabs(l.tabs);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].dir.as_deref(), Some(t.as_str()));
        assert!(tabs[0].cmdline.is_empty());
    }

    #[test]
    fn multiple_directories_become_one_tab_each() {
        let a = std::env::temp_dir().to_string_lossy().to_string();
        let l = parse(v(&[&a, &a])).unwrap();
        let tabs = expand_dir_tabs(l.tabs);
        assert_eq!(tabs.len(), 2);
        assert!(tabs.iter().all(|t| t.dir.as_deref() == Some(a.as_str())));
    }

    #[test]
    fn commands_and_explicit_dir_are_untouched() {
        let tmp = std::env::temp_dir().to_string_lossy().to_string();
        // A real command stays a command.
        let l = parse(v(&["ping", "localhost"])).unwrap();
        assert_eq!(expand_dir_tabs(l.tabs.clone()), l.tabs);
        // Explicit -d wins: the positional stays a command line even if it
        // happens to name a directory.
        let l = parse(v(&["nt", "-d", &tmp, &tmp])).unwrap();
        assert_eq!(expand_dir_tabs(l.tabs.clone()), l.tabs);
    }

    #[test]
    fn help_is_reported_via_err() {
        assert!(parse(v(&["--help"])).unwrap_err().contains("wt 互換"));
    }
}
