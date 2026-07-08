//! SSH/PTY spawning — the app-side half of the old terminal::pty_session.
//!
//! The engine crate (rikka-terminal) deliberately knows nothing about SSH
//! clients or settings; it exposes `build_terminal_session` over any
//! Read/Write pair. This module owns the shogun-desktop-specific transports:
//! native russh channels and ssh.exe through portable-pty (ConPTY).

use std::io::{Read, Write};
use std::sync::{Arc, atomic::Ordering};

use parking_lot::FairMutex;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::native_ssh::NativeSshClient;
use crate::ssh::{SshClient, SystemSshClient};
use crate::terminal::{PtyResizer, TerminalSession, pty_session::build_terminal_session};

use anyhow::Result;

/// Prepended to every `tmux attach-session` so tmux forwards the active pane's
/// title (with `#{pane_title}` verbatim, no chrome) to us. Without `set-titles
/// on` the title never leaves tmux, so the title-spinner progress fallback
/// (agents that drop OSC 9;4 in tmux — see `window::terminal_progress`) has
/// nothing to read. Single-quoted only, so it survives the Windows
/// `cmd.exe /c ssh …` path (no inner double quotes to break cmd's quoting).
pub const TMUX_ATTACH_TITLE_PREFIX: &str =
    "tmux set -g set-titles on && tmux set -g set-titles-string '#{pane_title}' && ";

/// XTVERSION identity from settings — honest by default, or a deliberate
/// Ghostty masquerade (see `settings::TerminalIdentity`).
fn xtversion_identity() -> String {
    crate::settings::load_settings()
        .unwrap_or_default()
        .terminal
        .identity
        .xtversion()
}

/// `(TERM_PROGRAM, TERM_PROGRAM_VERSION)` to inject.
fn term_program_identity() -> (&'static str, &'static str) {
    crate::settings::load_settings()
        .unwrap_or_default()
        .terminal
        .identity
        .term_program_env()
}

/// Whether to ask the remote tmux to forward pane titles (for the title-spinner
/// progress fallback). Default on; the settings toggle turns it off.
fn tmux_forward_titles() -> bool {
    crate::settings::load_settings()
        .unwrap_or_default()
        .terminal
        .tmux_forward_titles
}

// ── system-SSH resizer ────────────────────────────────────────────────────────

/// Newtype wrapper that asserts `Box<dyn MasterPty>` is `Send + Sync`.
///
/// # Safety
/// On Windows, portable-pty's ConPTY backend (`ConPtyMaster`) stores a Windows
/// `HPCON` handle.  Windows HANDLEs are reference-counted objects that may be
/// used from any thread; the Windows documentation explicitly states that
/// `ResizePseudoConsole` (which `MasterPty::resize` maps to) is thread-safe.
/// On Unix, the master file descriptor is guarded below by a `FairMutex`, which
/// prevents concurrent syscalls and makes the usage safe.
struct SendMaster(Box<dyn portable_pty::MasterPty>);
unsafe impl Send for SendMaster {}
unsafe impl Sync for SendMaster {}

struct SystemResizer {
    master: FairMutex<SendMaster>,
}

impl PtyResizer for SystemResizer {
    fn resize(
        &self,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> anyhow::Result<()> {
        self.master.lock().0.resize(PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        })?;
        Ok(())
    }
}

// ── public entry points ───────────────────────────────────────────────────────

/// Open a plain interactive shell on the SSH server, with the working directory
/// set to `project_path`. Unlike `spawn`, this does **not** attach to a tmux
/// session — it gives a raw interactive shell suitable for htop, vim, etc.
pub fn spawn_shell(
    ssh: &SshClient,
    project_path: &str,
    cols: u16,
    rows: u16,
    control_path: Option<String>,
) -> Result<TerminalSession> {
    match ssh {
        SshClient::Native(client) => spawn_shell_native(client, project_path, cols, rows),
        SshClient::System(client) => {
            spawn_shell_system(client, project_path, cols, rows, control_path)
        }
    }
}

fn spawn_shell_native(
    client: &NativeSshClient,
    project_path: &str,
    cols: u16,
    rows: u16,
) -> Result<TerminalSession> {
    let (prog, ver) = term_program_identity();
    let (reader, writer, resizer) =
        client.open_shell_channel(prog, ver, project_path, tmux_forward_titles(), cols, rows)?;
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> = Arc::new(FairMutex::new(writer));
    build_terminal_session(
        cols,
        rows,
        reader,
        writer,
        Arc::from(resizer),
        &xtversion_identity(),
    )
}

fn spawn_shell_system(
    ssh: &SystemSshClient,
    project_path: &str,
    cols: u16,
    rows: u16,
    control_path: Option<String>,
) -> Result<TerminalSession> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    #[cfg(windows)]
    let mut cmd = {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg("ssh");
        c
    };
    #[cfg(not(windows))]
    let mut cmd = CommandBuilder::new("ssh");

    cmd.arg("-t");
    cmd.args(["-p", &ssh.port.to_string()]);
    if ssh.ctrl_enabled.load(Ordering::Relaxed) {
        if let Some(ctrl) = control_path {
            cmd.args([
                "-o",
                "ControlMaster=auto",
                "-o",
                &format!("ControlPath={ctrl}"),
                "-o",
                "ControlPersist=30",
            ]);
        }
    }
    cmd.args(["-o", "ConnectTimeout=10"]);
    // ssh sends the local TERM in its pty-req; pin it to the configured name.
    cmd.env(
        "TERM",
        crate::settings::load_settings()
            .unwrap_or_default()
            .terminal
            .term
            .as_str(),
    );
    let (prog, ver) = term_program_identity();
    // cmd.env() covers local PTY use (e.g. non-SSH terminals sharing this crate).
    // The remote command uses the ZDOTDIR-wrapper integration so TERM_PROGRAM
    // survives login-shell startup scripts that would otherwise clear it.
    cmd.env("TERM_PROGRAM", prog);
    cmd.env("TERM_PROGRAM_VERSION", ver);
    cmd.env("COLORTERM", "truecolor");
    if let Some(ref key) = ssh.key_path {
        cmd.args(["-i", key]);
    }
    cmd.arg(format!("{}@{}", ssh.user, ssh.host));
    cmd.arg(crate::shell_integration::shell_window_cmd(
        prog,
        ver,
        project_path,
        tmux_forward_titles(),
    ));

    let _child = pair.slave.spawn_command(cmd)?;
    let writer_box: Box<dyn Write + Send> = pair.master.take_writer()?;
    let reader: Box<dyn Read + Send> = Box::new(pair.master.try_clone_reader()?);
    let resizer: Arc<dyn PtyResizer> = Arc::new(SystemResizer {
        master: FairMutex::new(SendMaster(pair.master)),
    });
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> = Arc::new(FairMutex::new(writer_box));
    build_terminal_session(cols, rows, reader, writer, resizer, &xtversion_identity())
}

pub fn spawn(
    ssh: &SshClient,
    tmux_session: &str,
    cols: u16,
    rows: u16,
    control_path: Option<String>,
) -> Result<TerminalSession> {
    match ssh {
        SshClient::Native(client) => spawn_native(client, tmux_session, cols, rows),
        SshClient::System(client) => spawn_system(client, tmux_session, cols, rows, control_path),
    }
}

fn spawn_native(
    client: &NativeSshClient,
    tmux_session: &str,
    cols: u16,
    rows: u16,
) -> Result<TerminalSession> {
    let (reader, writer, resizer) =
        client.open_pty_channel(tmux_session, tmux_forward_titles(), cols, rows)?;
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> = Arc::new(FairMutex::new(writer));
    build_terminal_session(
        cols,
        rows,
        reader,
        writer,
        Arc::from(resizer),
        &xtversion_identity(),
    )
}

fn spawn_system(
    ssh: &SystemSshClient,
    tmux_session: &str,
    cols: u16,
    rows: u16,
    control_path: Option<String>,
) -> Result<TerminalSession> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // On Windows, spawning ssh.exe directly via ConPTY can trigger
    // 0xc0000142 (STATUS_DLL_INIT_FAILED). Routing through cmd.exe lets
    // the console subsystem initialise correctly before ssh.exe starts.
    #[cfg(windows)]
    let mut cmd = {
        let mut c = CommandBuilder::new("cmd.exe");
        c.arg("/c");
        c.arg("ssh");
        c
    };
    #[cfg(not(windows))]
    let mut cmd = CommandBuilder::new("ssh");

    cmd.arg("-t");
    cmd.args(["-p", &ssh.port.to_string()]);
    if ssh.ctrl_enabled.load(Ordering::Relaxed) {
        if let Some(ctrl) = control_path {
            cmd.args([
                "-o",
                "ControlMaster=auto",
                "-o",
                &format!("ControlPath={ctrl}"),
                "-o",
                "ControlPersist=30",
            ]);
        }
    }
    cmd.args(["-o", "ConnectTimeout=10"]);
    // ssh sends the local TERM in its pty-req; pin it to the configured name.
    cmd.env(
        "TERM",
        crate::settings::load_settings()
            .unwrap_or_default()
            .terminal
            .term
            .as_str(),
    );
    let (prog, ver) = term_program_identity();
    cmd.env("TERM_PROGRAM", prog);
    cmd.env("TERM_PROGRAM_VERSION", ver);
    cmd.env("COLORTERM", "truecolor");
    if let Some(ref key) = ssh.key_path {
        cmd.args(["-i", key]);
    }
    // PTY sessions are interactive: ssh prompts for the password via the
    // terminal directly. SSH_ASKPASS is for headless exec only — do not set it here.
    cmd.arg(format!("{}@{}", ssh.user, ssh.host));
    let env_prefix = crate::shell_integration::remote_env_prefix(prog, ver);
    let title_prefix = if tmux_forward_titles() {
        TMUX_ATTACH_TITLE_PREFIX
    } else {
        ""
    };
    cmd.arg(format!(
        "{env_prefix}{title_prefix}tmux attach-session -t {tmux_session}"
    ));

    let _child = pair.slave.spawn_command(cmd)?;

    // Extract writer and reader from the master, then wrap the master itself in
    // the resizer so future `resize()` calls reach the OS PTY.
    let writer_box: Box<dyn Write + Send> = pair.master.take_writer()?;
    let reader: Box<dyn Read + Send> = Box::new(pair.master.try_clone_reader()?);
    let resizer: Arc<dyn PtyResizer> = Arc::new(SystemResizer {
        master: FairMutex::new(SendMaster(pair.master)),
    });

    let writer: Arc<FairMutex<Box<dyn Write + Send>>> = Arc::new(FairMutex::new(writer_box));
    build_terminal_session(cols, rows, reader, writer, resizer, &xtversion_identity())
}
