//! Local tabs born handoff-shaped (IPC.md "Local tabs on the handoff shape"):
//! instead of hiding ConPTY inside portable-pty, drive the vendored winconpty
//! (conpty.dll) directly, so a locally-spawned session owns the same handle
//! set an OS default-terminal handoff delivers. That is what makes a local
//! tab movable across window processes with the existing six-slot `attach`.
//!
//! The recipe, verified against microsoft/terminal (src/winconpty/*.cpp and
//! ConptyConnection.cpp):
//! 1. two anonymous pipe pairs — conpty duplicates the ends we hand it, so
//!    ours stay non-inheritable and the peers close right after create;
//! 2. `ConptyCreatePseudoConsole` from the dll beside the exe (the pair the
//!    sixel sideload already ships; OpenConsole.exe resolves beside the dll);
//! 3. client spawn with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, handle
//!    inheritance OFF — kernelbase carries the console reference through the
//!    attribute, reading it straight out of the HPCON struct;
//! 4. lift `hSignal`/`hConPtyProcess` out of the HPCON — its layout is
//!    declared "part of an ABI shared with the rest of the operating system"
//!    in winconpty.h, which is the same guarantee kernelbase relies on —
//!    then release the reference (upstream parity: `ConptyConnection::Start`)
//!    and close the husk. Our duplicates keep the pipes alive; dropping the
//!    session closes them, conhost sees signal EOF, and the console dies —
//!    the same kill switch the old portable-pty master drop provided.

#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use rikka_terminal_core::TerminalSession;
use rikka_terminal_core::pty_handoff::{HandoffPty, build_handoff_session};
use windows::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, FreeLibrary, HANDLE, HMODULE,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};
use windows::core::{HRESULT, PCWSTR, PWSTR};

/// winconpty's `HPCON`: an opaque pointer to [`PseudoConsoleAbi`].
type Hpcon = *mut c_void;

/// `COORD` without pulling in the Win32_System_Console feature.
#[repr(C)]
struct Coord {
    x: i16,
    y: i16,
}

/// winconpty.h `PseudoConsole` — quoting its header: "This structure is part
/// of an ABI shared with the rest of the operating system" (kernelbase reads
/// `reference` when it processes `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`).
/// That declared stability is what licenses lifting handles back out.
#[repr(C)]
struct PseudoConsoleAbi {
    signal: HANDLE,
    reference: HANDLE,
    conhost: HANDLE,
}

type CreateFn = unsafe extern "system" fn(Coord, HANDLE, HANDLE, u32, *mut Hpcon) -> HRESULT;
type ReleaseFn = unsafe extern "system" fn(Hpcon) -> HRESULT;
type CloseFn = unsafe extern "system" fn(Hpcon);

/// The three winconpty entry points we drive. Loaded once, never unloaded.
pub struct ConptyDll {
    create: CreateFn,
    release: ReleaseFn,
    close: CloseFn,
}

impl ConptyDll {
    /// Load `conpty.dll` from `dir`. OpenConsole.exe must sit beside it —
    /// winconpty resolves its console host relative to its own module path.
    pub fn load(dir: &Path) -> Result<ConptyDll> {
        let path = dir.join("conpty.dll");
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
            .with_context(|| format!("LoadLibraryW({})", path.display()))?;
        match Self::bind(module) {
            Ok(dll) => Ok(dll), // module stays loaded for the process lifetime
            Err(e) => {
                unsafe { FreeLibrary(module) }.ok();
                Err(e)
            }
        }
    }

    fn bind(module: HMODULE) -> Result<ConptyDll> {
        let sym = |name: &'static str| -> Result<unsafe extern "system" fn() -> isize> {
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(0);
            unsafe { GetProcAddress(module, windows::core::PCSTR(bytes.as_ptr())) }
                .ok_or_else(|| anyhow!("conpty.dll lacks {name}"))
        };
        // SAFETY: the winconpty exports have had these exact signatures since
        // the dll first shipped; the vendored copy is version-pinned.
        unsafe {
            Ok(ConptyDll {
                create: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateFn>(sym(
                    "ConptyCreatePseudoConsole",
                )?),
                release: std::mem::transmute::<unsafe extern "system" fn() -> isize, ReleaseFn>(
                    sym("ConptyReleasePseudoConsole")?,
                ),
                close: std::mem::transmute::<unsafe extern "system" fn() -> isize, CloseFn>(sym(
                    "ConptyClosePseudoConsole",
                )?),
            })
        }
    }

    /// The dll beside our exe — where build.rs (and the installer) put the
    /// sideloaded pair.
    fn beside_exe() -> Result<&'static ConptyDll> {
        static DLL: OnceLock<Result<ConptyDll, String>> = OnceLock::new();
        DLL.get_or_init(|| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let dir = exe.parent().ok_or("exe has no parent dir")?;
            ConptyDll::load(dir).map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
    }
}

/// Ensures `ConptyClosePseudoConsole` runs on every path. On success it runs
/// after the lift + release, closing only the husk's own copies.
struct Husk<'a>(&'a ConptyDll, Hpcon);

impl Drop for Husk<'_> {
    fn drop(&mut self) {
        unsafe { (self.0.close)(self.1) };
    }
}

/// Spawn `program args…` on a directly-driven ConPTY and assemble the engine
/// session over the lifted handles. The result is indistinguishable from an
/// adopted default-terminal handoff.
pub fn spawn_local(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
) -> Result<TerminalSession> {
    spawn_with(
        ConptyDll::beside_exe()?,
        program,
        args,
        cwd,
        cols,
        rows,
        crate::spawn_xtversion(),
    )
}

fn spawn_with(
    dll: &ConptyDll,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
    identity: &str,
) -> Result<TerminalSession> {
    ensure!(cols >= 1 && rows >= 1, "conpty rejects zero dimensions");

    // Input: we write, conhost reads. Output: conhost writes, we read.
    // ConptyCreatePseudoConsole duplicates the ends we pass (inheritable,
    // for its conhost spawn), so the peers can close right after.
    let (their_input, our_input) = std::io::pipe().context("input pipe")?;
    let (our_output, their_output) = std::io::pipe().context("output pipe")?;

    let mut hpcon: Hpcon = std::ptr::null_mut();
    unsafe {
        (dll.create)(
            Coord {
                x: cols as i16,
                y: rows as i16,
            },
            HANDLE(their_input.as_raw_handle()),
            HANDLE(their_output.as_raw_handle()),
            0,
            &mut hpcon,
        )
    }
    .ok()
    .context("ConptyCreatePseudoConsole")?;
    ensure!(!hpcon.is_null(), "ConptyCreatePseudoConsole returned null");
    let husk = Husk(dll, hpcon);
    drop((their_input, their_output));

    // The client rides PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE; the reference
    // inside the HPCON must still be alive here (kernelbase duplicates it
    // into the child), so the spawn happens before the release below.
    let client = launch_client(hpcon, program, args, cwd)?;

    let abi = unsafe { &*(hpcon as *const PseudoConsoleAbi) };
    let signal = dup(abi.signal).context("lift signal out of HPCON")?;
    let conhost = dup(abi.conhost).context("lift conhost process out of HPCON")?;

    // Upstream parity (ConptyConnection::Start): release the reference the
    // moment the connection is set up, so conhost exits once its last client
    // does; then close the husk — only its own copies die, our lifts live.
    unsafe { (dll.release)(hpcon) }
        .ok()
        .context("ConptyReleasePseudoConsole")?;
    drop(husk);

    build_handoff_session(
        cols,
        rows,
        HandoffPty {
            input: our_input.into(),
            output: our_output.into(),
            signal: Some(signal),
            keepalive: vec![conhost, client],
        },
        identity,
    )
}

/// CreateProcessW with the pseudoconsole attribute — the mirror of
/// microsoft/terminal's `ConptyConnection::_LaunchAttachedClient`: handle
/// inheritance OFF, `STARTF_USESTDHANDLES` with no handles (so the child
/// gets the console's std handles, not stray copies of ours), the HPCON
/// value passed directly as the attribute payload.
fn launch_client(
    hpcon: Hpcon,
    program: &str,
    args: &[String],
    cwd: Option<&str>,
) -> Result<OwnedHandle> {
    let mut cmdline: Vec<u16> = {
        let mut s = String::new();
        append_quoted(program, &mut s);
        for arg in args {
            s.push(' ');
            append_quoted(arg, &mut s);
        }
        s.encode_utf16().chain([0]).collect()
    };
    let cwd_wide: Option<Vec<u16>> =
        cwd.map(|d| std::ffi::OsStr::new(d).encode_wide().chain([0]).collect());
    // TERM/COLORTERM tell cross-platform TUIs (btop, ncurses apps) our color
    // depth and capabilities — konsole/alacritty set them too. Native console
    // apps (cmd/pwsh) ignore TERM, so this is harmless for them. TERM_PROGRAM
    // follows the configured identity (honest, or a ghostty masquerade).
    let (term_program, term_program_version) = crate::spawn_term_program();
    let env = env_block_with(&[
        ("TERM", crate::spawn_term()),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", term_program),
        ("TERM_PROGRAM_VERSION", term_program_version),
    ]);

    let mut size: usize = 0;
    // First call fails by design — it reports the required size.
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    let mut buf = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut c_void);
    unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
        .context("InitializeProcThreadAttributeList")?;
    let spawned = (|| -> Result<PROCESS_INFORMATION> {
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                // Quirk of this attribute (and upstream usage): the HPCON
                // value itself is the payload pointer.
                Some(hpcon),
                std::mem::size_of::<Hpcon>(),
                None,
                None,
            )
        }
        .context("UpdateProcThreadAttribute(PSEUDOCONSOLE)")?;

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut pi = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                Some(env.as_ptr() as *const c_void),
                cwd_wide
                    .as_ref()
                    .map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr())),
                &si.StartupInfo,
                &mut pi,
            )
        }
        .with_context(|| format!("CreateProcessW({program})"))?;
        Ok(pi)
    })();
    unsafe { DeleteProcThreadAttributeList(list) };
    let pi = spawned?;
    if !pi.hThread.is_invalid() {
        drop(unsafe { OwnedHandle::from_raw_handle(pi.hThread.0) });
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(pi.hProcess.0) })
}

/// Same-process handle duplication into an owned wrapper.
fn dup(h: HANDLE) -> Result<OwnedHandle> {
    if h.is_invalid() || h.0.is_null() {
        bail!("HPCON member handle is absent");
    }
    let mut out = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            h,
            GetCurrentProcess(),
            &mut out,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .context("DuplicateHandle")?;
    Ok(unsafe { OwnedHandle::from_raw_handle(out.0) })
}

/// Append one argument under CommandLineToArgvW's rules: bare when no
/// whitespace or quote; otherwise quoted, with N backslashes before a quote
/// becoming 2N+1 and trailing backslashes doubled.
fn append_quoted(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// The parent environment with `overrides` upserted (case-insensitive names,
/// Windows semantics), serialized as the CreateProcessW Unicode block:
/// `name=value\0`… terminated by an extra `\0`, sorted by uppercased name as
/// the docs require.
fn env_block_with(overrides: &[(&str, &str)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars().collect();
    for (name, value) in overrides {
        match vars.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some((_, v)) => *v = value.to_string(),
            None => vars.push((name.to_string(), value.to_string())),
        }
    }
    vars.sort_by_key(|(n, _)| n.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (name, value) in &vars {
        block.extend(name.encode_utf16());
        block.extend("=".encode_utf16());
        block.extend(value.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize the conhost-spawning tests: parallel cold starts crowd
    /// each other out badly enough to blow ANY reasonable deadline on this
    /// machine (0.77s alone, 30s+ under contention) — growing the deadline
    /// was a losing race. Ignore poisoning: a panicked test already failed
    /// on its own; the next one should still run serialized.
    static CONHOST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn conhost_serial() -> std::sync::MutexGuard<'static, ()> {
        CONHOST.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn quoted(arg: &str) -> String {
        let mut s = String::new();
        append_quoted(arg, &mut s);
        s
    }

    #[test]
    fn quoting_matches_argv_rules() {
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted(r"C:\dir\file"), r"C:\dir\file");
        assert_eq!(quoted("two words"), r#""two words""#);
        assert_eq!(quoted(""), r#""""#);
        assert_eq!(quoted(r#"say "hi""#), r#""say \"hi\"""#);
        // Bare backslashes need no quoting at all…
        assert_eq!(quoted(r"end\"), r"end\");
        // …but double before the closing quote once quoting kicks in,
        assert_eq!(quoted(r"tail \"), r#""tail \\""#);
        // and 2N+1 before an embedded quote.
        assert_eq!(quoted(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quoted(r"mid\dle space"), r#""mid\dle space""#);
    }

    #[test]
    fn env_block_upserts_sorts_and_terminates() {
        let block = env_block_with(&[("RIKKA_ZZ_TEST", "on"), ("TERM_PROGRAM", "x")]);
        assert_eq!(block.last(), Some(&0));
        let s = String::from_utf16(&block[..block.len() - 1]).unwrap();
        let entries: Vec<&str> = s.split('\0').filter(|e| !e.is_empty()).collect();
        assert!(entries.contains(&"RIKKA_ZZ_TEST=on"));
        let terms: Vec<&str> = entries
            .iter()
            .copied()
            .filter(|e| e.to_uppercase().starts_with("TERM_PROGRAM="))
            .collect();
        assert_eq!(terms, ["TERM_PROGRAM=x"], "override must not duplicate");
        let names: Vec<String> = entries
            .iter()
            .map(|e| e.split('=').next().unwrap().to_uppercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "block must be name-sorted");
    }

    /// Diagnostic probe (not a test): what does the vendored conpty emit on
    /// a resize? Determines whether resize desync self-heals (conhost
    /// repaints) or accumulates (quirk-era silence). Run with
    /// `cargo test conpty_resize_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn conpty_resize_probe() {
        use std::io::{Read as _, Write as _};

        use rikka_terminal_core::pty_handoff::resize_signal_packet;
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");

        let (their_input, mut our_input) = std::io::pipe().unwrap();
        let (our_output, their_output) = std::io::pipe().unwrap();
        let mut hpcon: Hpcon = std::ptr::null_mut();
        unsafe {
            (dll.create)(
                Coord { x: 80, y: 24 },
                HANDLE(their_input.as_raw_handle()),
                HANDLE(their_output.as_raw_handle()),
                0,
                &mut hpcon,
            )
        }
        .ok()
        .unwrap();
        drop((their_input, their_output));
        let client = launch_client(
            hpcon,
            "cmd.exe",
            &[
                "/k".into(),
                "for /L %i in (1,1,40) do @echo LINE-%i-x".into(),
            ],
            None,
        )
        .unwrap();
        let abi = unsafe { &*(hpcon as *const PseudoConsoleAbi) };
        let mut signal: std::fs::File = dup(abi.signal).unwrap().into();
        unsafe { (dll.release)(hpcon) }.ok().unwrap();
        unsafe { (dll.close)(hpcon) };

        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut out: std::fs::File = OwnedHandle::from(our_output).into();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let collect = |label: &str| {
            let mut bytes = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(chunk) => bytes.extend(chunk),
                    Err(_) => break,
                }
            }
            let text: String = String::from_utf8_lossy(&bytes).escape_debug().to_string();
            println!("=== {label}: {} bytes ===\n{text}\n", bytes.len());
        };
        collect("initial paint");
        collect("initial paint (late)");
        // Resize storm, like a window drag: many sizes in quick succession.
        for (c, r) in [
            (78, 23),
            (75, 22),
            (70, 21),
            (66, 20),
            (60, 18),
            (55, 17),
            (50, 16),
            (45, 15),
            (52, 17),
            (61, 19),
            (73, 22),
            (85, 26),
            (94, 28),
            (100, 30),
        ] {
            signal.write_all(&resize_signal_packet(c, r)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        collect("after resize storm to 100x30");
        // conhost's own truth: its size numbers and the absolute cursor
        // positions it uses while echoing.
        our_input.write_all(b"mode con\r").unwrap();
        collect("mode con response");
        drop(client);
    }

    /// Diagnostic probe (not a test): REAL yazi under the vendored conpty,
    /// quit with `q` — does the main screen come back (`?1049l`)?
    /// Root cause of the 2026-07-16 stuck-alt-screen bug, established by
    /// this probe's variant bisection: answering yazi's kitty keyboard
    /// query (`CSI ? u` → `?0u`) makes yazi use the protocol's push/pop,
    /// and OpenConsole 1.24.2605.12001 then swallows the tail of the exit
    /// restore burst — `?1049l` never reaches the terminal (wt is immune
    /// because it never answers the query). The engine fix: `mark_conpty`
    /// disables the advertisement, so "no ?u reply" is rikka's shipping
    /// behavior and must show `1049l=true`.
    /// Needs yazi installed. Run with
    /// `cargo test alt_exit_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn alt_exit_probe() {
        use std::io::{Read as _, Write as _};

        for (label, u_reply) in [
            // The very first conhost of a run dies at startup whatever the
            // variant (cold-start flake) — burn it on a warmup.
            ("warmup (ignore)", false),
            ("no ?u reply (the fix) — expect 1049l=true", false),
            ("?0u reply (the bug) — conhost eats the teardown", true),
        ] {
            let _serial = conhost_serial();
            let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
            let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
            let (their_input, mut our_input) = std::io::pipe().unwrap();
            let (our_output, their_output) = std::io::pipe().unwrap();
            let mut hpcon: Hpcon = std::ptr::null_mut();
            unsafe {
                (dll.create)(
                    Coord { x: 80, y: 24 },
                    HANDLE(their_input.as_raw_handle()),
                    HANDLE(their_output.as_raw_handle()),
                    0,
                    &mut hpcon,
                )
            }
            .ok()
            .unwrap();
            drop((their_input, their_output));
            // Answer the DA1 query conhost sends at startup (the engine
            // answers `?6c`). The reply can sit in the input pipe ahead of
            // the request — the input state machine parses it on arrival.
            our_input.write_all(b"\x1b[?6c").unwrap();
            // INTERACTIVE pwsh, driven by "typing" into the input pipe —
            // the live failure has an interactive shell (PSReadLine's
            // console-API traffic keeps conhost in its rendering mode)
            // hosting the TUI child; -Command runs put conhost in a pure
            // streaming mode that forwards everything and hides the bug.
            let client = launch_client(
                hpcon,
                "pwsh.exe",
                &["-NoProfile".into(), "-NoLogo".into()],
                None,
            )
            .unwrap();
            unsafe { (dll.release)(hpcon) }.ok().unwrap();
            unsafe { (dll.close)(hpcon) };

            let mut bytes = Vec::new();
            let mut out: std::fs::File = OwnedHandle::from(our_output).into();
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            // Let the prompt come up, then "type" the yazi launch.
            std::thread::sleep(std::time::Duration::from_secs(3));
            // Real yazi, driven like a user: launch, let it draw, quit.
            let yazi = if std::path::Path::new(r"C:\Users\aki\scoop\shims\yazi.exe").exists() {
                r"& 'C:\Users\aki\scoop\shims\yazi.exe'"
            } else {
                "yazi"
            };
            let _ = our_input.write_all(format!("{yazi}\r").as_bytes());
            let mut sent_q = false;
            let mut q_at: Option<std::time::Instant> = None;
            let mut answered = std::collections::HashSet::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left.min(std::time::Duration::from_millis(400))) {
                    Ok(chunk) => bytes.extend(chunk),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                // Answer yazi's runtime detection queries the way the live
                // engine does; only the kitty keyboard reply is the
                // variant under test.
                let mut pairs: Vec<(&[u8], &[u8])> = vec![
                    (b"\x1b[>q", b"\x1bP>|rikka-terminal 0.1.0\x1b\\"),
                    (b"\x1b]11;?", b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\"),
                    (b"\x1b[16t", b"\x1b[6;20;10t"),
                    (b"\x1b[0c", b"\x1b[?6c"),
                    (b"\x1b_Gi=31", b"\x1b_Gi=31;OK\x1b\\"),
                ];
                if u_reply {
                    pairs.push((b"\x1b[?u", b"\x1b[?0u"));
                }
                for (query, reply) in pairs {
                    if !answered.contains(query) && bytes.windows(query.len()).any(|w| w == query) {
                        let _ = our_input.write_all(reply);
                        answered.insert(query);
                    }
                }
                if !sent_q && bytes.windows(8).any(|w| w == b"\x1b[?1049h") {
                    // Give yazi a beat to settle, then quit it.
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    while let Ok(chunk) = rx.try_recv() {
                        bytes.extend(chunk);
                    }
                    let _ = our_input.write_all(b"q");
                    sent_q = true;
                    q_at = Some(std::time::Instant::now());
                    continue;
                }
                if sent_q && bytes.windows(8).any(|w| w == b"\x1b[?1049l") {
                    break; // restored — success shape
                }
                // Earlier probe runs proved a single 'q' can be swallowed
                // (yazi still alive minutes later) — retry every 2s.
                if let Some(t) = q_at
                    && t.elapsed() > std::time::Duration::from_secs(2)
                {
                    let _ = our_input.write_all(b"q");
                    q_at = Some(std::time::Instant::now());
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(800));
            while let Ok(chunk) = rx.try_recv() {
                bytes.extend(chunk);
            }
            let has = |pat: &[u8]| bytes.windows(pat.len()).any(|w| w == pat);
            println!(
                "=== {label}: {} bytes | 1049h={} 1049l={} ===",
                bytes.len(),
                has(b"\x1b[?1049h"),
                has(b"\x1b[?1049l"),
            );
            println!("{}\n", String::from_utf8_lossy(&bytes).escape_debug());
            drop(client);
        }
    }

    /// Diagnostic probe (not a test): pure-WIDTH storm at fixed height,
    /// through a prompt row long enough to wrap at the narrow sizes.
    /// Any post-storm row drift between conhost's echoes and our grid is
    /// wrap-reflow asymmetry, isolated from the (fixed) growth anchoring.
    /// Run with `cargo test width_reflow_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn width_reflow_probe() {
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session = spawn_with(
            &dll,
            "cmd.exe",
            &[
                "/k".into(),
                "for /L %i in (1,1,40) do @echo LINE-%i-x".into(),
            ],
            None,
            80,
            24,
            "test-identity",
        )
        .expect("spawn cmd");
        let dump = |label: &str| {
            let snap = session.snapshot.lock();
            println!(
                "--- {label} ({}x{}) ---",
                snap.cells[0].len(),
                snap.cells.len()
            );
            for (i, row) in snap.cells.iter().enumerate() {
                let text: String = row.iter().map(|c| c.c).collect();
                let text = text.trim_end();
                if !text.is_empty() {
                    println!("{i:2}| {text}");
                }
            }
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let hit = session.snapshot.lock().cells.iter().any(|r| {
                r.iter()
                    .map(|c| c.c)
                    .collect::<String>()
                    .contains("release>")
            });
            if hit || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        dump("before storm");

        for c in [70u16, 60, 50, 45, 50, 60, 70, 80] {
            session.resize(c, 24, (8.0, 16.0));
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
        dump("after width storm back to 80x24");

        session.send_bytes(b"echo Z\r");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        dump("after echo Z");
    }

    /// The width-resize invariant distilled from `width_semantics_probe`:
    /// conhost truncates rows on a shrink and never restores them on a
    /// grow — our grid now does the same, so after a shrink+grow round
    /// trip the next echo must land ON the prompt row. Before the fix our
    /// reflow drifted the row accounting and the echo overwrote stale
    /// rows one line off ("BB\Users\aki\…" mid-row was the signature).
    #[test]
    fn width_shrink_grow_keeps_conhost_agreement() {
        let _serial = conhost_serial();
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session = spawn_with(
            &dll,
            "cmd.exe",
            &["/k".into(), "for /L %i in (1,1,5) do @echo SHORT-%i".into()],
            None,
            80,
            24,
            "test-identity",
        )
        .expect("spawn cmd");
        let rows_text = || -> Vec<String> {
            session
                .snapshot
                .lock()
                .cells
                .iter()
                .map(|row| row.iter().map(|c| c.c).collect::<String>())
                .collect()
        };
        let wait_for = |needle: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !rows_text().iter().any(|r| r.contains(needle)) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{needle:?} never appeared"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait_for("SHORT-5");

        session.resize(45, 24, (8.0, 16.0));
        std::thread::sleep(std::time::Duration::from_millis(400));
        session.send_bytes(b"echo AA\r");
        wait_for("AA");

        session.resize(80, 24, (8.0, 16.0));
        std::thread::sleep(std::time::Duration::from_millis(400));
        session.send_bytes(b"echo BB\r");
        wait_for("BB");

        let rows = rows_text();
        assert!(
            rows.iter()
                .any(|r| r.contains("rikka-terminal>") && r.contains("echo BB")),
            "echo must land on the prompt row, rows: {:?}",
            rows.iter()
                .map(|r| r.trim_end())
                .filter(|r| !r.is_empty())
                .collect::<Vec<_>>()
        );
        assert!(
            !rows.iter().any(|r| r.contains("BB\\Users")),
            "echo output overwrote a stale row — the drift signature"
        );
        session.send_bytes(b"exit\r");
    }

    /// Diagnostic probe (not a test): does the ConPTY conhost REFLOW on a
    /// width shrink (wrapping long rows onto extra lines) or truncate them
    /// in place (the RESIZE_QUIRK-now-default hypothesis)? And does a grow
    /// restore what a shrink did? One shrink, one grow, an echo after
    /// each — the echo's landing row exposes conhost's row accounting.
    /// Run: cargo test width_semantics_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn width_semantics_probe() {
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session = spawn_with(
            &dll,
            "cmd.exe",
            &["/k".into(), "for /L %i in (1,1,5) do @echo SHORT-%i".into()],
            None,
            80,
            24,
            "test-identity",
        )
        .expect("spawn cmd");
        let dump = |label: &str| {
            let snap = session.snapshot.lock();
            println!(
                "--- {label} ({}x{}) ---",
                snap.cells[0].len(),
                snap.cells.len()
            );
            for (i, row) in snap.cells.iter().enumerate() {
                let text: String = row.iter().map(|c| c.c).collect();
                let text = text.trim_end();
                if !text.is_empty() {
                    println!("{i:2}| {text}");
                }
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let done = session.snapshot.lock().cells.iter().any(|r| {
                r.iter()
                    .map(|c| c.c)
                    .collect::<String>()
                    .contains("SHORT-5")
            });
            if done || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        dump("before (80 cols, ~62-char prompt fits)");

        session.resize(45, 24, (8.0, 16.0));
        std::thread::sleep(std::time::Duration::from_millis(400));
        session.send_bytes(b"echo AA\r");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        dump("after shrink to 45 + echo AA");

        session.resize(80, 24, (8.0, 16.0));
        std::thread::sleep(std::time::Duration::from_millis(400));
        session.send_bytes(b"echo BB\r");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        dump("after grow back to 80 + echo BB");
        session.send_bytes(b"exit\r");
    }

    /// Diagnostic probe (not a test): what does conhost do on a PURE
    /// vertical grow — with content overflowing the viewport (rows already
    /// scrolled out) vs fitting inside it? Determines the anchoring a
    /// just-merged tab must reflow with when the receiving window is
    /// TALLER than the sender (the "input jumps to the bottom" report).
    /// Run: cargo test vertical_grow_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn vertical_grow_probe() {
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        for lines in [10u16, 40] {
            let session = spawn_with(
                &dll,
                "cmd.exe",
                &[
                    "/k".into(),
                    format!("for /L %i in (1,1,{lines}) do @echo LINE-%i-x"),
                ],
                None,
                80,
                24,
                "test-identity",
            )
            .expect("spawn cmd");
            let dump = |label: &str| {
                let snap = session.snapshot.lock();
                println!(
                    "--- {label} ({}x{}, cursor row {}) ---",
                    snap.cells[0].len(),
                    snap.cells.len(),
                    snap.cursor.1,
                );
                for (i, row) in snap.cells.iter().enumerate() {
                    let text: String = row.iter().map(|c| c.c).collect();
                    let text = text.trim_end();
                    if !text.is_empty() {
                        println!("{i:2}| {text}");
                    }
                }
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                let done = session.snapshot.lock().cells.iter().any(|r| {
                    r.iter()
                        .map(|c| c.c)
                        .collect::<String>()
                        .contains(&format!("LINE-{lines}-x"))
                });
                if done || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            dump(&format!("{lines} lines, before grow (24 rows)"));
            session.resize(80, 40, (8.0, 16.0));
            std::thread::sleep(std::time::Duration::from_millis(400));
            session.send_bytes(b"echo ZZZ-MARKER\r");
            std::thread::sleep(std::time::Duration::from_millis(1200));
            dump(&format!("{lines} lines, after grow to 40 + echo"));
            session.send_bytes(b"exit\r");
        }
    }

    /// The resize-storm probe distilled into an invariant: after a
    /// shrink+grow storm, conhost echoes typed input at absolute
    /// coordinates computed from ITS layout (it emits nothing during the
    /// storm itself). With conhost-anchored growth our grid matches, so the
    /// echo must land on the row that carries the prompt — not mid-listing,
    /// which is what users saw as "typed text appears in the middle of the
    /// screen after resizing a lot".
    #[test]
    fn resize_storm_keeps_conhost_and_grid_in_agreement() {
        let _serial = conhost_serial();
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session = spawn_with(
            &dll,
            "cmd.exe",
            &[
                "/k".into(),
                "for /L %i in (1,1,40) do @echo LINE-%i-x".into(),
            ],
            None,
            80,
            24,
            "test-identity",
        )
        .expect("spawn cmd");
        let rows_text = || -> Vec<String> {
            session
                .snapshot
                .lock()
                .cells
                .iter()
                .map(|row| row.iter().map(|c| c.c).collect::<String>())
                .collect()
        };

        // Per-phase deadlines: parallel test runs spawn several consoles at
        // once, and a shared budget starves the later phases when conhost
        // cold starts crowd each other.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !rows_text().iter().any(|r| r.contains("LINE-40-x"))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            rows_text().iter().any(|r| r.contains("LINE-40-x")),
            "listing never finished (conhost start starved?)"
        );

        // Storm like a window drag. Widths stay >= 70 so the ~62-char
        // prompt+echo row never wraps: wrap-reflow parity with conhost is a
        // separate (unfixed) axis; this pins the row-anchoring fix.
        for (c, r) in [
            (78, 23),
            (76, 22),
            (74, 21),
            (72, 20),
            (70, 18),
            (70, 16),
            (70, 15),
            (72, 17),
            (75, 19),
            (80, 22),
            (85, 26),
            (94, 28),
            (100, 30),
        ] {
            session.resize(c, r, (8.0, 16.0));
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        session.send_bytes(b"mode con\r");
        let mut echo_row = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if let Some(row) = rows_text().iter().find(|r| r.contains("mode con")) {
                echo_row = Some(row.clone());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let echo_row = echo_row.expect("echo never appeared");
        assert!(
            echo_row.contains("rikka-terminal>"),
            "echo must land on the prompt row, got {echo_row:?}"
        );
        session.send_bytes(b"exit\r");
    }

    /// Resizes must actually land in the console, not just in our grid:
    /// drive an interactive cmd, resize over the lifted signal pipe, and let
    /// `mode con` report the console's own idea of its dimensions. The
    /// signal and input pipes are separate lanes with no cross-ordering
    /// guarantee, so the probe re-runs until the console catches up.
    #[test]
    fn resize_reaches_the_console() {
        let _serial = conhost_serial();
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session =
            spawn_with(&dll, "cmd.exe", &[], None, 80, 24, "test-identity").expect("spawn cmd");
        let grid = || -> String {
            session
                .snapshot
                .lock()
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|c| c.c))
                .collect()
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while grid().trim().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!grid().trim().is_empty(), "cmd must show a prompt");

        session.resize(97, 43, (8.0, 16.0));
        let mut reported = false;
        while std::time::Instant::now() < deadline {
            session.send_bytes(b"mode con\r");
            std::thread::sleep(std::time::Duration::from_millis(700));
            let g = grid();
            // `mode con` prints the counts as bare numbers in any locale.
            if g.contains("97") && g.contains("43") {
                reported = true;
                break;
            }
        }
        assert!(reported, "console must report 97x43 after the resize");
        session.send_bytes(b"exit\r");
    }

    /// Full drive of the vendored pair: create → attribute spawn → lift →
    /// release → close husk → engine session. The marker proves output and
    /// the env block; the disconnect proves the released reference lets
    /// conhost exit with its client — the whole point of being born
    /// handoff-shaped.
    #[test]
    fn local_spawn_is_born_handoff_shaped() {
        let _serial = conhost_serial();
        let assets = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/conpty"));
        let dll = ConptyDll::load(assets).expect("vendored conpty.dll");
        let session = spawn_with(
            &dll,
            "cmd.exe",
            &["/c".into(), "echo LIVE:%TERM_PROGRAM%".into()],
            None,
            80,
            24,
            "test-identity",
        )
        .expect("handoff-shaped local spawn");

        // Per-phase deadlines (not one shared budget): parallel test runs
        // spawn several consoles at once and cold starts crowd each other.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = false;
        while std::time::Instant::now() < deadline {
            let grid: String = session
                .snapshot
                .lock()
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|c| c.c))
                .collect();
            if grid.contains("LIVE:rikka-terminal") {
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(seen, "marker with TERM_PROGRAM must reach the grid");

        session.resize(100, 30, (8.0, 16.0));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !session.is_connected(),
            "client exit must break the pipes (reference released)"
        );
    }
}
