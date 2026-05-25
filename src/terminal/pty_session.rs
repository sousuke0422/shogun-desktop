use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use alacritty_terminal::{
    event::VoidListener,
    term::{test::TermSize, Config},
    vte::ansi::{Processor, StdSyncHandler},
    Term,
};
use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::ssh::SshClient;
use crate::terminal::{take_snapshot, GridSnapshot, TerminalSession};

pub fn spawn(
    ssh: &SshClient,
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
    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer()?));

    let term = Arc::new(Mutex::new(Term::new(
        Config::default(),
        &TermSize::new(cols as usize, rows as usize),
        VoidListener,
    )));
    let snapshot = Arc::new(Mutex::new(GridSnapshot::blank(cols as usize, rows as usize)));
    let connected = Arc::new(AtomicBool::new(true));
    let generation = Arc::new(AtomicU64::new(0));
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    {
        let term2 = Arc::clone(&term);
        let snap2 = Arc::clone(&snapshot);
        let conn2 = Arc::clone(&connected);
        let gen2 = Arc::clone(&generation);
        let err2 = Arc::clone(&error);
        let mut reader = pair.master.try_clone_reader()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut parser = Processor::<StdSyncHandler>::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        conn2.store(false, Ordering::Relaxed);
                        *err2.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some("PTY接続が切断されました".into());
                        break;
                    }
                    Ok(n) => {
                        let mut t = term2.lock().unwrap_or_else(|e| e.into_inner());
                        parser.advance(&mut *t, &buf[..n]);
                        *snap2.lock().unwrap_or_else(|e| e.into_inner()) = take_snapshot(&t);
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
    })
}
