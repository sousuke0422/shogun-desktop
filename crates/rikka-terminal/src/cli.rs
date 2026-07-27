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
//! - internal: `--attach in,out,signal,ref,server,client` (plus
//!   `--attach-title`, and `--size` for the requested cells) — the
//!   default-terminal cold start (IPC.md "attach cold"). Emitted by
//!   rikka-handoff.exe with inherited handle values; never typed by hand
//! - internal: `--window-process` — this instance was spawned by the running
//!   monarch to host one window (crash isolation): skip the election, never
//!   forward, register back instead. Never typed by hand
//!
//! Also speaks the common-core flags of Linux terminal emulators (xterm /
//! gnome-terminal / alacritty / kitty), groundwork for the P3 Linux port:
//! `-e cmd args…` (rest-of-argv command), `-- cmd args…`, `--working-directory`,
//! `-t/-T/--title`, `--hold` (accepted; today's no-restart behavior already
//! holds), `--maximize`/`--full-screen`, `--geometry CxR[+X+Y]`,
//! `--class`/`--name` (accepted, no X11 here), `-v/--version`.
//!
//! Also: `-w/--window new|last|0|<id>` — route the launch's tabs into an
//! existing window (`0`/`last` = any live window; `<id>` = a per-window id).
//! The monarch resolves the target and forwards the spawn to that window's
//! own socket; anything unresolvable falls open to a fresh window.
//!
//! Deliberately TODO (structural work, tracked in README):
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
    /// `--hold` (alacritty/kitty): keep the tab after the command exits.
    /// Accepted for compatibility — sessions already freeze-and-stay today;
    /// becomes meaningful once exit-closes-tab lands.
    pub hold: bool,
    /// Color-scheme name for this tab (from its profile's `colorScheme`);
    /// resolved through `wt_schemes` at tab creation for per-tab theming.
    pub color_scheme: Option<String>,
}

/// Internal: an OS default-terminal handoff riding in this launch (the cold
/// start of IPC.md "attach cold"). Set by rikka-handoff.exe; the handle
/// values are valid in THIS process via CreateProcess handle inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSpec {
    /// Raw handle values in wire order: input, output, signal, reference,
    /// server, client. `0` = absent (input/output are never 0).
    pub handles: [i64; 6],
    /// Startup title from the handoff's TERMINAL_STARTUP_INFO.
    pub title: Option<String>,
    /// `--attach-state`: a temp file holding the sender's screen replay
    /// (raw VT bytes) for a cross-process tab detach — read once and
    /// deleted by the child. Handles ride inheritance; bulk bytes cannot,
    /// hence the file.
    pub state_path: Option<String>,
    /// `--attach-palette`: the tab's color palette (19 packed 0xRRGGBB — see
    /// `AttachArgs::palette`), so a detached tab keeps its profile colors.
    pub palette: Option<Vec<u32>>,
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
    /// `--attach`: a cold-start handoff. When set, the launch opens the
    /// adopted session instead of any `tabs`.
    pub attach: Option<AttachSpec>,
    /// `--window-process`: spawned by the monarch to host one window — skip
    /// the single-instance election and register with the spawner instead.
    pub window_process: bool,
    /// `-w/--window`: open this launch's tabs in an EXISTING window.
    /// `Some(0)` = any window (`-w 0` / `-w last`); `Some(id)` = that window
    /// (per-window ids). `None` = a fresh window (default, and `-w new`).
    pub window: Option<u64>,
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

/// `--attach` value: exactly six comma-separated handle values.
fn parse_attach(s: &str) -> Result<AttachSpec, String> {
    let vals: Option<Vec<i64>> = s.split(',').map(|p| p.trim().parse().ok()).collect();
    let handles: [i64; 6] = vals
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| format!("--attach は in,out,signal,ref,server,client の6値です: {s}"))?;
    if handles[0] == 0 || handles[1] == 0 {
        return Err("--attach: input/output ハンドルは必須です".into());
    }
    Ok(AttachSpec {
        handles,
        title: None,
        state_path: None,
        palette: None,
    })
}

/// Parse `rt` arguments (argv without the program name).
pub fn parse(args: Vec<String>) -> Result<Launch, String> {
    let mut launch = Launch::default();
    let mut attach_title: Option<String> = None;
    let mut attach_state: Option<String> = None;
    let mut attach_palette: Option<Vec<u32>> = None;

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
                    "-F" | "--fullscreen" | "--full-screen" => {
                        launch.fullscreen = true;
                        it.next();
                    }
                    "--maximize" => {
                        launch.maximized = true;
                        it.next();
                    }
                    "--geometry" => {
                        it.next();
                        let v = value_of(&mut it, "--geometry")?;
                        let g = parse_geometry(&v)?;
                        launch.size_cells = Some((g.0, g.1));
                        if let Some(pos) = g.2 {
                            launch.pos = Some(pos);
                        }
                    }
                    "-v" | "--version" => {
                        // Same x.y.z-hash string the About page shows, so a
                        // screenshot and a shell both name the same build.
                        return Err(format!(
                            "RikkaTerminal {}",
                            crate::settings_window::version_string()
                        ));
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
                        it.next();
                        let v = value_of(&mut it, "-w/--window")?;
                        launch.window = match v.as_str() {
                            // wt semantics: "new" forces a fresh window (our
                            // default anyway); 0/"last" reuses an existing one.
                            "new" => None,
                            "last" | "0" => Some(0),
                            id => Some(id.parse::<u64>().map_err(|_| {
                                format!("-w/--window は new / last / 0 / 窓ID のいずれかです: {id}")
                            })?),
                        };
                    }
                    // Internal (rikka-handoff.exe): default-terminal cold
                    // start — see AttachSpec.
                    "--attach" => {
                        it.next();
                        let v = value_of(&mut it, "--attach")?;
                        launch.attach = Some(parse_attach(&v)?);
                    }
                    "--attach-title" => {
                        it.next();
                        attach_title = Some(value_of(&mut it, "--attach-title")?);
                    }
                    "--attach-state" => {
                        it.next();
                        attach_state = Some(value_of(&mut it, "--attach-state")?);
                    }
                    "--attach-palette" => {
                        it.next();
                        let v = value_of(&mut it, "--attach-palette")?;
                        let vals: Result<Vec<u32>, _> =
                            v.split(',').map(|t| u32::from_str_radix(t, 16)).collect();
                        attach_palette =
                            Some(vals.map_err(|_| "--attach-palette: 16進CSVが不正".to_string())?);
                    }
                    // Internal (the monarch): window-process mode — see
                    // Launch::window_process.
                    "--window-process" => {
                        launch.window_process = true;
                        it.next();
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
                "-d" | "--startingDirectory" | "--working-directory" | "--directory" => {
                    spec.dir = Some(value_of(&mut it, "-d")?)
                }
                "-t" | "-T" | "--title" => spec.title = Some(value_of(&mut it, "--title")?),
                // xterm/alacritty `-e` and gnome-terminal/foot `--`: the REST
                // of the arguments are the command, verbatim.
                "-e" | "--command" | "--" => {
                    spec.cmdline.extend(it.by_ref());
                }
                "--hold" => spec.hold = true,
                // X11 concepts, accepted so Linux-style invocations don't
                // error out on Windows.
                "--class" | "--name" => {
                    let _ = value_of(&mut it, &a)?;
                }
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

    match (
        &mut launch.attach,
        attach_title,
        attach_state,
        attach_palette,
    ) {
        (Some(a), title, state, palette) => {
            a.title = title;
            a.state_path = state;
            a.palette = palette;
        }
        (None, None, None, None) => {}
        (None, _, _, _) => {
            return Err(
                "--attach-title/--attach-state/--attach-palette は --attach と共に使います".into(),
            );
        }
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
                    hold: spec.hold,
                    color_scheme: spec.color_scheme.clone(),
                })
                .collect()
        })
        .collect()
}

/// xterm-style `--geometry COLSxROWS[+X+Y]`. Negative (edge-relative)
/// offsets are not supported.
fn parse_geometry(s: &str) -> Result<(u16, u16, Option<(f32, f32)>), String> {
    let err = || format!("--geometry は \"CxR\" または \"CxR+X+Y\" 形式です: {s}");
    let (size, rest) = match s.find(['+', '-']) {
        Some(i) => (&s[..i], Some(&s[i..])),
        None => (s, None),
    };
    let (c, r) = size.split_once(['x', 'X']).ok_or_else(err)?;
    let c: u16 = c.trim().parse().map_err(|_| err())?;
    let r: u16 = r.trim().parse().map_err(|_| err())?;
    let pos = match rest {
        None => None,
        Some(rest) => {
            let parts: Vec<&str> = rest.split('+').filter(|p| !p.is_empty()).collect();
            if rest.contains('-') || parts.len() != 2 {
                return Err(err());
            }
            let x: f32 = parts[0].trim().parse().map_err(|_| err())?;
            let y: f32 = parts[1].trim().parse().map_err(|_| err())?;
            Some((x, y))
        }
    };
    Ok((c, r, pos))
}

pub const HELP: &str = "rt — RikkaTerminal (wt 互換 CLI)\n\n\
rt [options] [new-tab [tab-options] [command...]] [; new-tab ...]\n\
rt <dir> [...]  — code コマンド流: そのディレクトリでシェルを開く (1個1タブ)\n\n\
options:\n  -M, --maximized      最大化で起動\n  -F, --fullscreen     フルスクリーンで起動\n  \
--pos x,y            ウィンドウ位置 (px)\n  --size c,r           サイズ (セル数)\n\n\
new-tab (nt):\n  -p, --profile <shell>          シェル (pwsh / powershell / cmd / 実行ファイル)\n  \
-d, --startingDirectory <dir>  開始ディレクトリ\n  --title <title>                初期タブタイトル\n  \
<command...>                   シェルの代わりに実行するコマンド\n\n\
Linux 互換 (xterm / gnome-terminal / alacritty / kitty の共通形):\n  \
-e <cmd> [args...] / -- <cmd> [args...]   以降全部をコマンドとして実行\n  \
--working-directory <dir>      開始ディレクトリ\n  -t / -T <title>                タイトル\n  \
--geometry CxR[+X+Y]           サイズ(セル)と位置\n  --maximize / --full-screen     最大化 / フルスクリーン\n  \
--hold / --class / --name      受理 (hold は現状常時相当)\n  -v, --version                  バージョン表示\n\n\
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
    fn window_target_forms() {
        // Default and the explicit "new" are both a fresh window.
        assert_eq!(parse(v(&[])).unwrap().window, None);
        assert_eq!(parse(v(&["-w", "new"])).unwrap().window, None);
        // 0 / "last" = any existing window.
        assert_eq!(parse(v(&["-w", "0"])).unwrap().window, Some(0));
        assert_eq!(parse(v(&["--window", "last"])).unwrap().window, Some(0));
        // A concrete per-window id.
        assert_eq!(
            parse(v(&["-w", "44040193"])).unwrap().window,
            Some(44040193)
        );
        // Garbage is an error, not a silent new window.
        assert!(parse(v(&["-w", "nope"])).is_err());
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
        // `-w` graduated from the TODO list: it parses and carries a target.
        let l = parse(v(&["-w", "0", "nt"])).unwrap();
        assert_eq!(l.window, Some(0));
        assert_eq!(l.tabs.len(), 1);
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
    fn xterm_dash_e_takes_the_rest() {
        let l = parse(v(&["-e", "vim", "-R", "file.txt"])).unwrap();
        assert_eq!(l.tabs[0].cmdline, v(&["vim", "-R", "file.txt"]));
    }

    #[test]
    fn gnome_double_dash_takes_the_rest() {
        let l = parse(v(&[
            "--working-directory",
            "C:\\src",
            "--",
            "htop",
            "-d",
            "5",
        ]))
        .unwrap();
        assert_eq!(l.tabs[0].dir.as_deref(), Some("C:\\src"));
        assert_eq!(l.tabs[0].cmdline, v(&["htop", "-d", "5"]));
    }

    #[test]
    fn linux_title_and_hold_flags() {
        let l = parse(v(&["-T", "監視", "--hold", "--class", "Rikka"])).unwrap();
        assert_eq!(l.tabs[0].title.as_deref(), Some("監視"));
        assert!(l.tabs[0].hold);
    }

    #[test]
    fn geometry_parses_size_and_position() {
        let l = parse(v(&["--geometry", "120x40+100+200"])).unwrap();
        assert_eq!(l.size_cells, Some((120, 40)));
        assert_eq!(l.pos, Some((100.0, 200.0)));
        let l = parse(v(&["--geometry", "80x24"])).unwrap();
        assert_eq!(l.size_cells, Some((80, 24)));
        assert_eq!(l.pos, None);
        assert!(parse(v(&["--geometry", "120x40-5+9"])).is_err());
        assert!(parse(v(&["--geometry", "big"])).is_err());
    }

    #[test]
    fn gnome_style_window_flags() {
        let l = parse(v(&["--maximize"])).unwrap();
        assert!(l.maximized);
        let l = parse(v(&["--full-screen"])).unwrap();
        assert!(l.fullscreen);
    }

    #[test]
    fn attach_parses_handles_title_and_size() {
        let l = parse(v(&[
            "--attach",
            "10,20,30,40,50,60",
            "--attach-title",
            "cmd",
            "--size",
            "120,40",
        ]))
        .unwrap();
        let a = l.attach.expect("attach spec");
        assert_eq!(a.handles, [10, 20, 30, 40, 50, 60]);
        assert_eq!(a.title.as_deref(), Some("cmd"));
        assert_eq!(l.size_cells, Some((120, 40)));
        assert!(l.tabs.is_empty());
    }

    #[test]
    fn window_process_flag_composes_with_a_normal_launch() {
        let l = parse(v(&["--window-process", "nt", "-p", "pwsh"])).unwrap();
        assert!(l.window_process);
        assert_eq!(l.tabs[0].profile.as_deref(), Some("pwsh"));
        // And with a relayed attach (the monarch's crash-isolation path).
        let l = parse(v(&["--window-process", "--attach", "1,2,0,0,0,0"])).unwrap();
        assert!(l.window_process);
        assert!(l.attach.is_some());
    }

    #[test]
    fn attach_rejects_malformed_values() {
        // Wrong arity, junk, and missing mandatory pipes all error.
        assert!(parse(v(&["--attach", "1,2,3"])).is_err());
        assert!(parse(v(&["--attach", "1,2,3,4,5,x"])).is_err());
        assert!(parse(v(&["--attach", "0,2,3,4,5,6"])).is_err());
        assert!(parse(v(&["--attach-title", "t"])).is_err());
    }

    #[test]
    fn version_reports_via_err() {
        assert!(parse(v(&["-v"])).unwrap_err().contains("RikkaTerminal"));
    }

    #[test]
    fn help_is_reported_via_err() {
        assert!(parse(v(&["--help"])).unwrap_err().contains("wt 互換"));
    }
}
