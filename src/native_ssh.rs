use crate::settings::ShogunDesktopSettings;
use crate::terminal::PtyResizer;
use anyhow::{bail, Context, Result};
use russh::client::{self, Handler};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, ChannelWriteHalf};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};

/// Pure-Rust SSH client backed by russh (lazy connect, session reuse).
#[derive(Clone)]
pub struct NativeSshClient {
    host: String,
    port: u16,
    user: String,
    key_path: Option<String>,
    password: Option<String>,
    proxy_command: Option<String>,
    accept_all_host_keys: bool,
    session: Arc<Mutex<Option<client::Handle<ShogunHandler>>>>,
    rt: Arc<tokio::runtime::Runtime>,
}

#[derive(Clone)]
struct ShogunHandler {
    host: String,
    port: u16,
    accept_all: bool,
}

impl ShogunHandler {
    fn new(host: &str, port: u16, accept_all: bool) -> Self {
        Self {
            host: host.to_string(),
            port,
            accept_all,
        }
    }
}

impl Handler for ShogunHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        if self.accept_all {
            return Ok(true);
        }
        check_or_accept_server_key(&self.host, self.port, server_public_key)
    }
}

impl NativeSshClient {
    pub fn new(settings: &ShogunDesktopSettings) -> Result<Self> {
        let ssh = &settings.ssh;
        let user = if !ssh.user.is_empty() {
            ssh.user.clone()
        } else {
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default()
        };
        if user.is_empty() && ssh.proxy_command.is_empty() {
            bail!("SSHユーザー名が未設定です");
        }
        let key_path = if !ssh.key_path.is_empty()
            && std::path::Path::new(&ssh.key_path).exists()
        {
            Some(ssh.key_path.clone())
        } else {
            None
        };
        let password = if !ssh.password.is_empty() {
            Some(ssh.password.clone())
        } else {
            None
        };
        let proxy_command = if !ssh.proxy_command.is_empty() {
            Some(ssh.proxy_command.clone())
        } else {
            None
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("tokio runtime の作成に失敗しました")?;
        Ok(Self {
            host: ssh.host.clone(),
            port: ssh.port,
            user,
            key_path,
            password,
            proxy_command,
            accept_all_host_keys: ssh.accept_all_host_keys,
            session: Arc::new(Mutex::new(None)),
            rt: Arc::new(rt),
        })
    }

    pub fn exec(&self, command: &str) -> Result<String> {
        self.rt
            .block_on(self.exec_async(command))
            .map_err(|e| self.reset_session_err(e))
    }

    pub fn is_connected(&self) -> bool {
        self.session
            .try_lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// Open a plain interactive shell channel (no tmux), with CWD set to `project_path`.
    pub fn open_shell_channel(
        &self,
        project_path: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>, Box<dyn PtyResizer>)> {
        let cmd = format!("cd {project_path} && exec $SHELL -l");
        let (reader, writer, resizer) = self
            .rt
            .block_on(self.open_pty_async(&cmd, cols, rows))
            .map_err(|e| self.reset_session_err(e))?;
        Ok((Box::new(reader), Box::new(writer), Box::new(resizer)))
    }

    pub fn open_pty_channel(
        &self,
        tmux_session: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>, Box<dyn PtyResizer>)> {
        let cmd = format!("tmux attach-session -t {tmux_session}");
        let (reader, writer, resizer) = self
            .rt
            .block_on(self.open_pty_async(&cmd, cols, rows))
            .map_err(|e| self.reset_session_err(e))?;
        Ok((Box::new(reader), Box::new(writer), Box::new(resizer)))
    }

    async fn open_pty_async(
        &self,
        command: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(MpscChannelReader, ChannelSyncWriter, NativeResizer)> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let handle = self.connect_async().await?;
            *guard = Some(handle);
        }
        let session = guard
            .as_mut()
            .expect("session initialized above");
        let channel = session
            .channel_open_session()
            .await
            .context("SSH セッションチャンネルのオープンに失敗しました")?;
        channel
            .request_pty(
                false,
                "xterm-256color",
                cols as u32,
                rows as u32,
                0,
                0,
                &[],
            )
            .await
            .context("SSH PTY 要求に失敗しました")?;
        channel
            .exec(true, command)
            .await
            .context("SSH exec の開始に失敗しました")?;

        let (read_half, write_half) = channel.split();
        let (tx, rx) = mpsc::unbounded_channel();

        // Share the write half between the writer and the resizer.
        let write_half_arc = Arc::new(Mutex::new(write_half));

        self.rt.spawn(async move {
            let mut read_half = read_half;
            loop {
                match read_half.wait().await {
                    Some(ChannelMsg::Data { data }) => {
                        if tx.send(data.to_vec()).is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                        if tx.send(data.to_vec()).is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExitStatus { .. })
                    | Some(ChannelMsg::Eof)
                    | Some(ChannelMsg::Close)
                    | None => {
                        break;
                    }
                    _ => {}
                }
            }
        });

        let writer = ChannelSyncWriter {
            write_half: Arc::clone(&write_half_arc),
            rt: Arc::clone(&self.rt),
        };
        let resizer = NativeResizer {
            write_half: write_half_arc,
            rt: Arc::clone(&self.rt),
        };

        Ok((
            MpscChannelReader {
                rx,
                buf: Vec::new(),
                pos: 0,
            },
            writer,
            resizer,
        ))
    }

    async fn exec_async(&self, command: &str) -> Result<String> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let handle = self.connect_async().await?;
            *guard = Some(handle);
        }
        let session = guard
            .as_mut()
            .expect("session initialized above");
        let mut channel = session
            .channel_open_session()
            .await
            .context("SSH セッションチャンネルのオープンに失敗しました")?;
        channel
            .exec(true, command)
            .await
            .context("SSH exec の開始に失敗しました")?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext } if ext == 1 => {
                    stderr.extend_from_slice(&data)
                }
                _ => {}
            }
        }

        let mut combined = String::from_utf8_lossy(&stdout).into_owned();
        if !stderr.is_empty() {
            let err = String::from_utf8_lossy(&stderr);
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&err);
        }
        Ok(combined)
    }

    async fn connect_async(&self) -> Result<client::Handle<ShogunHandler>> {
        let config = Arc::new(client::Config::default());
        let handler = ShogunHandler::new(&self.host, self.port, self.accept_all_host_keys);
        let mut handle = if let Some(ref proxy_cmd) = self.proxy_command {
            let expanded = proxy_cmd
                .replace("%h", &self.host)
                .replace("%p", &self.port.to_string());

            #[cfg(unix)]
            let child_result = tokio::process::Command::new("sh")
                .args(["-c", &expanded])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            #[cfg(windows)]
            let child_result = tokio::process::Command::new("cmd")
                .args(["/c", &expanded])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();

            let mut child = child_result.context("ProxyCommand の起動に失敗しました")?;
            let stdin = child.stdin.take().context("ProxyCommand stdin 取得失敗")?;
            let stdout = child.stdout.take().context("ProxyCommand stdout 取得失敗")?;
            let stream = tokio::io::join(stdout, stdin);

            client::connect_stream(config, stream, handler)
                .await
                .context("ProxyCommand 経由の SSH 接続に失敗しました")?
        } else {
            let addrs = (self.host.as_str(), self.port);
            client::connect(config, addrs, handler)
                .await
                .context("SSH 接続に失敗しました")?
        };

        if let Some(ref key_path) = self.key_path {
            let key_pair = load_secret_key(key_path, None)
                .with_context(|| format!("秘密鍵の読み込みに失敗しました: {key_path}"))?;
            let auth_res = handle
                .authenticate_publickey(
                    &self.user,
                    PrivateKeyWithHashAlg::new(
                        Arc::new(key_pair),
                        handle.best_supported_rsa_hash().await?.flatten(),
                    ),
                )
                .await
                .context("公開鍵認証に失敗しました")?;
            if !auth_res.success() {
                bail!("公開鍵認証が拒否されました");
            }
        } else if let Some(ref password) = self.password {
            let auth_res = handle
                .authenticate_password(&self.user, password)
                .await
                .context("パスワード認証に失敗しました")?;
            if !auth_res.success() {
                bail!("パスワード認証が拒否されました");
            }
        } else {
            bail!("SSH認証情報が未設定です（鍵またはパスワードが必要）");
        }

        Ok(handle)
    }

    fn reset_session_err(&self, err: impl Into<anyhow::Error>) -> anyhow::Error {
        if let Ok(mut guard) = self.session.try_lock() {
            *guard = None;
        } else {
            self.rt.block_on(async {
                *self.session.lock().await = None;
            });
        }
        err.into()
    }
}

struct MpscChannelReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for MpscChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            self.buf = match self.rx.blocking_recv() {
                Some(data) => data,
                None => return Ok(0),
            };
            self.pos = 0;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let available = self.buf.len() - self.pos;
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct ChannelSyncWriter {
    write_half: Arc<Mutex<ChannelWriteHalf<client::Msg>>>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl Write for ChannelSyncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let data = buf.to_vec();
        self.rt
            .block_on(async {
                let guard = self.write_half.lock().await;
                let mut writer = guard.make_writer();
                writer.write_all(&data).await
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// PTY resizer for the native russh backend.
///
/// Sends an SSH `window-change` request on the existing channel so that the
/// remote pty (and tmux) can reflow to the new dimensions.
struct NativeResizer {
    write_half: Arc<Mutex<ChannelWriteHalf<client::Msg>>>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl PtyResizer for NativeResizer {
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        let write_half = Arc::clone(&self.write_half);
        self.rt.block_on(async move {
            let guard = write_half.lock().await;
            guard
                .window_change(cols as u32, rows as u32, 0, 0)
                .await
                .map_err(|e| anyhow::anyhow!("SSH window_change failed: {:?}", e))
        })
    }
}

fn known_hosts_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("known_hosts")
}

fn host_aliases(host: &str, port: u16) -> Vec<String> {
    let mut aliases = vec![host.to_string(), format!("{host}:{port}")];
    if port != 22 {
        aliases.push(format!("[{host}]:{port}"));
    }
    aliases
}

fn public_key_fields(key: &PublicKey) -> Result<(String, String)> {
    let line = key
        .to_openssh()
        .context("サーバー公開鍵のエンコードに失敗しました")?;
    let mut parts = line.split_whitespace();
    let algo = parts
        .next()
        .context("公開鍵アルゴリズムがありません")?
        .to_string();
    let blob = parts
        .next()
        .context("公開鍵データがありません")?
        .to_string();
    Ok((algo, blob))
}

fn line_matches_host(hosts_field: &str, aliases: &[String]) -> bool {
    if hosts_field.starts_with('|') {
        return false;
    }
    hosts_field.split(',').any(|entry| {
        aliases.iter().any(|alias| entry == alias)
    })
}

fn line_key_matches(line: &str, algo: &str, blob: &str) -> bool {
    let mut parts = line.split_whitespace();
    let _hosts = parts.next();
    let line_algo = parts.next();
    let line_blob = parts.next();
    line_algo == Some(algo) && line_blob == Some(blob)
}

fn check_or_accept_server_key(
    host: &str,
    port: u16,
    server_public_key: &PublicKey,
) -> Result<bool, russh::Error> {
    let path = known_hosts_path();
    let aliases = host_aliases(host, port);
    let (algo, blob) = public_key_fields(server_public_key)
        .map_err(|e| russh::Error::InvalidConfig(e.to_string()))?;

    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(russh::Error::IO)?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if !line_matches_host(trimmed.split_whitespace().next().unwrap_or(""), &aliases) {
                continue;
            }
            if line_key_matches(trimmed, &algo, &blob) {
                return Ok(true);
            }
            return Err(russh::Error::KeyChanged { line: 0 });
        }
    }

    append_known_host(&path, host, port, &algo, &blob)
        .map_err(|e| russh::Error::IO(std::io::Error::other(e.to_string())))?;
    Ok(true)
}

fn append_known_host(
    path: &Path,
    host: &str,
    port: u16,
    algo: &str,
    blob: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let line = format!("{host_field} {algo} {blob}\n");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_aliases_includes_bracketed_port() {
        let aliases = host_aliases("example.com", 2222);
        assert!(aliases.contains(&"example.com".to_string()));
        assert!(aliases.contains(&"[example.com]:2222".to_string()));
    }

    #[test]
    fn line_matches_host_plain_and_bracketed() {
        let aliases = host_aliases("myhost", 22);
        assert!(line_matches_host("myhost", &aliases));
        assert!(line_matches_host("myhost,other", &aliases));
        assert!(!line_matches_host("|1|abcdef", &aliases));
    }

    #[test]
    fn line_key_matches_compares_algo_and_blob() {
        let line = "myhost ssh-ed25519 AAAAB3NzaC1lZDI1NTE5AAAAI test";
        assert!(line_key_matches(line, "ssh-ed25519", "AAAAB3NzaC1lZDI1NTE5AAAAI"));
        assert!(!line_key_matches(line, "ssh-rsa", "AAAAB3NzaC1lZDI1NTE5AAAAI"));
    }
}
