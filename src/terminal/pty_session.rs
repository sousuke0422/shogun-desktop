use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
};

use parking_lot::FairMutex;

use alacritty_terminal::{
    Term,
    term::{Config, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};
use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::native_ssh::NativeSshClient;
use crate::ssh::{SshClient, SystemSshClient};
use crate::terminal::{
    ClipboardEvent, ClipboardListener, GridSnapshot, PtyResizer, TerminalSession, take_snapshot,
};

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
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master.lock().0.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
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
    build_terminal_session(cols, rows, reader, writer, Arc::from(resizer))
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
    build_terminal_session(cols, rows, reader, writer, resizer)
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
    build_terminal_session(cols, rows, reader, writer, Arc::from(resizer))
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
    build_terminal_session(cols, rows, reader, writer, resizer)
}

fn build_terminal_session(
    cols: u16,
    rows: u16,
    reader: Box<dyn Read + Send>,
    writer: Arc<FairMutex<Box<dyn Write + Send>>>,
    resizer: Arc<dyn PtyResizer>,
) -> Result<TerminalSession> {
    // ── OSC 52 clipboard handler ──────────────────────────────────────────────
    // Channel capacity of 16 is enough to absorb bursts without blocking the
    // PTY reader thread. Events are silently dropped when the buffer is full.
    let (cb_tx, cb_rx) = std::sync::mpsc::sync_channel::<ClipboardEvent>(16);
    let writer_for_cb = Arc::clone(&writer);
    std::thread::spawn(move || {
        while let Ok(event) = cb_rx.recv() {
            match event {
                ClipboardEvent::Store(text) => {
                    // Write application text → host clipboard.
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(&text);
                    }
                }
                ClipboardEvent::Load(callback) => {
                    // Read host clipboard → generate OSC 52 response → write to PTY.
                    let content = arboard::Clipboard::new()
                        .and_then(|mut cb| cb.get_text())
                        .unwrap_or_default();
                    let response = callback(&content);
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(response.as_bytes());
                }
                ClipboardEvent::PtyWrite(text) => {
                    // Generic write-back (OSC color queries etc.).
                    let mut w = writer_for_cb.lock();
                    let _ = w.write_all(text.as_bytes());
                }
            }
        }
    });

    let listener = ClipboardListener { tx: cb_tx };
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &TermSize::new(cols as usize, rows as usize),
        listener,
    )));
    let snapshot = Arc::new(FairMutex::new(GridSnapshot::blank(
        cols as usize,
        rows as usize,
    )));
    let connected = Arc::new(AtomicBool::new(true));
    let generation = Arc::new(AtomicU64::new(0));
    let error: Arc<FairMutex<Option<String>>> = Arc::new(FairMutex::new(None));

    {
        let term2 = Arc::clone(&term);
        let snap2 = Arc::clone(&snapshot);
        let conn2 = Arc::clone(&connected);
        let gen2 = Arc::clone(&generation);
        let err2 = Arc::clone(&error);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            let mut parser = Processor::<StdSyncHandler>::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        conn2.store(false, Ordering::Relaxed);
                        *err2.lock() = Some("PTY接続が切断されました".into());
                        break;
                    }
                    Ok(n) => {
                        let mut t = term2.lock();
                        for &byte in &buf[..n] {
                            parser.advance(&mut *t, byte);
                        }
                        *snap2.lock() = take_snapshot(&t);
                        gen2.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }

    Ok(TerminalSession {
        term,
        writer,
        snapshot,
        connected,
        generation,
        error,
        cols: AtomicU16::new(cols),
        rows: AtomicU16::new(rows),
        resizer,
    })
}

/// Convenience constructor for tests that do not need a real PTY.
#[cfg(test)]
pub fn build_test_session(cols: u16, rows: u16) -> TerminalSession {
    use crate::terminal::NoopResizer;
    use std::io::Cursor;
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
        Arc::new(FairMutex::new(Box::new(std::io::sink())));
    let reader: Box<dyn Read + Send> = Box::new(Cursor::new(vec![]));
    build_terminal_session(cols, rows, reader, writer, Arc::new(NoopResizer)).unwrap()
}
