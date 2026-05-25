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

pub fn spawn(ssh: &SshClient, tmux_session: &str, cols: u16, rows: u16) -> Result<TerminalSession> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("ssh");
    cmd.arg("-t");
    cmd.args(["-p", &ssh.port.to_string()]);
    if ssh.ctrl_enabled.load(Ordering::Relaxed) {
        let ctrl = ssh.control_socket_path();
        cmd.args([
            "-o",
            "ControlMaster=auto",
            "-o",
            &format!("ControlPath={ctrl}"),
            "-o",
            "ControlPersist=30",
        ]);
    }
    cmd.args(["-o", "ConnectTimeout=10"]);
    if let Some(ref key) = ssh.key_path {
        cmd.args(["-i", key]);
    }
    if let Some(ref pw) = ssh.password {
        if let Ok((script, pass)) = ssh.write_askpass_pub(pw) {
            cmd.env("SSH_ASKPASS", script.to_string_lossy().to_string());
            cmd.env("SSH_ASKPASS_REQUIRE", "force");
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                let _ = std::fs::remove_file(&script);
                let _ = std::fs::remove_file(&pass);
            });
        }
    }
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
                        *err2.lock().unwrap() = Some("PTY接続が切断されました".into());
                        break;
                    }
                    Ok(n) => {
                        let mut t = term2.lock().unwrap();
                        for &byte in &buf[..n] {
                            parser.advance(&mut *t, byte);
                        }
                        *snap2.lock().unwrap() = take_snapshot(&t);
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
