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
use crate::terminal::notify;
use crate::terminal::progress::{OscEvent, OscScanner, Progress};
use crate::terminal::{
    ClipboardEvent, ClipboardListener, GridSnapshot, PtyResizer, TerminalSession, kitty_graphics,
    take_snapshot,
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
    fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> anyhow::Result<()> {
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
    // OSC 52: alacritty's default is OnlyCopy — store works but load (the
    // clipboard *read* query, e.g. vim's "+p over SSH) is silently denied,
    // so the ClipboardEvent::Load path never fired. CopyPaste enables both.
    // Tradeoff: a remote app can read the host clipboard; acceptable for a
    // tool that only connects to our own infrastructure.
    let config = Config {
        osc52: alacritty_terminal::term::Osc52::CopyPaste,
        // Track kitty keyboard-protocol pushes/pops and answer the CSI ? u
        // query (reply arrives via Event::PtyWrite → ClipboardEvent). The
        // resulting TermMode bits drive the encoder in terminal::keys.
        kitty_keyboard: true,
        ..Config::default()
    };
    let term = Arc::new(FairMutex::new(Term::new(
        config,
        &TermSize::new(cols as usize, rows as usize),
        listener,
    )));
    let snapshot = Arc::new(FairMutex::new(GridSnapshot::blank(
        cols as usize,
        rows as usize,
    )));
    let connected = Arc::new(AtomicBool::new(true));
    let generation = Arc::new(AtomicU64::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let error: Arc<FairMutex<Option<String>>> = Arc::new(FairMutex::new(None));
    let progress = Arc::new(Progress::default());
    let notifications: notify::NotificationQueue = Default::default();
    let images = Arc::new(kitty_graphics::KittyImageStore::default());

    {
        let term2 = Arc::clone(&term);
        let snap2 = Arc::clone(&snapshot);
        let conn2 = Arc::clone(&connected);
        let gen2 = Arc::clone(&generation);
        let notify2 = Arc::clone(&notify);
        let err2 = Arc::clone(&error);
        let progress2 = Arc::clone(&progress);
        let notifications2 = Arc::clone(&notifications);
        let images2 = Arc::clone(&images);
        let writer2 = Arc::clone(&writer);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            let mut parser = Processor::<StdSyncHandler>::new();
            // OSC 9 / 9;4 / 777 observer — the vte stack drops these
            // sequences, so a passive side-scanner extracts progress and
            // desktop notifications (see terminal::progress / notify).
            let mut osc = OscScanner::new();
            // Kitty graphics: APC observer (vte swallows APC too) + protocol
            // driver. Responses (query replies, transmission ACKs) go back
            // to the PTY writer. See terminal::kitty_graphics.
            let mut apc = kitty_graphics::ApcScanner::new();
            let mut kitty = kitty_graphics::KittyGraphics::new(images2);
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        conn2.store(false, Ordering::Relaxed);
                        *err2.lock() = Some("PTY接続が切断されました".into());
                        // Bump + signal so the UI repaints the disconnected
                        // state promptly instead of waiting for other traffic.
                        gen2.fetch_add(1, Ordering::Relaxed);
                        notify2.notify_one();
                        break;
                    }
                    Ok(n) => {
                        let mut t = term2.lock();
                        let mut responses: Vec<Vec<u8>> = Vec::new();
                        for &byte in &buf[..n] {
                            match osc.advance(byte) {
                                Some(OscEvent::Progress(update)) => progress2.apply(update),
                                Some(OscEvent::Notify(note)) => notify::push(&notifications2, note),
                                None => {}
                            }
                            if let Some(payload) = apc.advance(byte)
                                && let Some(resp) = kitty.apply(&payload)
                            {
                                responses.push(resp);
                            }
                            parser.advance(&mut *t, byte);
                        }
                        *snap2.lock() = take_snapshot(&t);
                        drop(t);
                        // Written only after releasing the term lock, so the
                        // UI input path (which also takes the writer lock)
                        // never waits behind parsing + snapshotting.
                        if !responses.is_empty() {
                            let mut w = writer2.lock();
                            for resp in responses {
                                let _ = w.write_all(&resp);
                            }
                            let _ = w.flush();
                        }
                        gen2.fetch_add(1, Ordering::Relaxed);
                        notify2.notify_one();
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
        notify,
        error,
        progress,
        notifications,
        images,
        cols: AtomicU16::new(cols),
        rows: AtomicU16::new(rows),
        resizer,
    })
}

/// Convenience constructor for tests that do not need a real PTY.
#[cfg(test)]
pub fn build_test_session(cols: u16, rows: u16) -> TerminalSession {
    build_test_session_with_output(cols, rows, Vec::new())
}

/// Test session whose "PTY" plays back a canned output byte stream, then
/// EOFs — exercises the real reader thread (scanners, parser, snapshots).
#[cfg(test)]
pub fn build_test_session_with_output(cols: u16, rows: u16, output: Vec<u8>) -> TerminalSession {
    use crate::terminal::NoopResizer;
    use std::io::Cursor;
    let writer: Arc<FairMutex<Box<dyn Write + Send>>> =
        Arc::new(FairMutex::new(Box::new(std::io::sink())));
    let reader: Box<dyn Read + Send> = Box::new(Cursor::new(output));
    build_terminal_session(cols, rows, reader, writer, Arc::new(NoopResizer)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full kitty-graphics pipeline through the real reader thread:
    /// APC scanner → protocol driver → image store, and placeholder cells
    /// through vte → snapshot. Guards the integration seams the unit tests
    /// in kitty_graphics.rs / mod.rs cannot see.
    #[test]
    fn kitty_graphics_end_to_end_through_reader_thread() {
        use base64::Engine as _;

        // 1x1 red PNG, transmitted as a virtual placement (U=1, 1x1 cells).
        let mut png = Vec::new();
        let img = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

        let mut script = Vec::new();
        script.extend(format!("\x1b_Ga=T,U=1,i=3,f=100,c=1,r=1;{b64}\x1b\\").into_bytes());
        // Placeholder cell: fg = 256-color index 3 (image id), row/col 0.
        script.extend("\x1b[38;5;3m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".bytes());

        let session = build_test_session_with_output(10, 2, script);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.is_connected() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!session.is_connected(), "reader thread must reach EOF");

        let stored = session.images.get(3).expect("image must land in the store");
        assert_eq!((stored.cols, stored.rows), (1, 1));
        let snap = session.snapshot.lock().clone();
        assert!(snap.has_images);
        assert_eq!(
            snap.cells[0][0].image,
            Some(kitty_graphics::PlaceholderCell { id: 3, row: 0, col: 0 })
        );
    }
}
