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

/// XTVERSION identity from settings — honest by default, or a deliberate
/// Ghostty masquerade (see `settings::TerminalIdentity`).
fn xtversion_identity() -> String {
    crate::settings::load_settings()
        .unwrap_or_default()
        .terminal
        .identity
        .xtversion()
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
    let (reader, writer, resizer) = client.open_shell_channel(project_path, cols, rows)?;
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
    if let Some(ref key) = ssh.key_path {
        cmd.args(["-i", key]);
    }
    cmd.arg(format!("{}@{}", ssh.user, ssh.host));
    // cd to project, then exec the user's default shell
    cmd.arg(format!("cd {project_path} && exec $SHELL -l"));

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
    let (reader, writer, resizer) = client.open_pty_channel(tmux_session, cols, rows)?;
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
    if let Some(ref key) = ssh.key_path {
        cmd.args(["-i", key]);
    }
    // PTY sessions are interactive: ssh prompts for the password via the
    // terminal directly. SSH_ASKPASS is for headless exec only — do not set it here.
    cmd.arg(format!("{}@{}", ssh.user, ssh.host));
    cmd.arg(format!("tmux attach-session -t {tmux_session}"));

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
