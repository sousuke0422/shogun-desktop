use crate::settings::ShogunDesktopSettings;
use anyhow::{bail, Context as AnyhowContext, Result};
use ssh2::Session;
use std::io::Read;
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

const CMD_TIMEOUT_SECS: u64 = 30;

/// SSH authentication method.
#[derive(Debug, Clone)]
pub enum SshAuth {
    /// Path to a private key file.
    PrivateKey(String),
    /// Password authentication.
    Password(String),
}

/// SSH client for remote command execution (blocking I/O).
pub struct SshClient {
    session: Session,
}

impl SshClient {
    /// Establish an SSH session.
    pub fn connect(host: &str, port: u16, user: &str, key_or_pass: &SshAuth) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let tcp = TcpStream::connect(&addr).map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;
        tcp.set_read_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;

        let mut session = Session::new().context("SSHセッションの作成に失敗しました")?;
        session.set_tcp_stream(tcp);
        session.handshake().map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;

        match key_or_pass {
            SshAuth::PrivateKey(path) => {
                session
                    .userauth_pubkey_file(user, None, Path::new(path), None)
                    .map_err(|_| anyhow::anyhow!("ユーザー {user} の認証に失敗しました"))?;
            }
            SshAuth::Password(password) => {
                session
                    .userauth_password(user, password)
                    .map_err(|_| anyhow::anyhow!("ユーザー {user} の認証に失敗しました"))?;
            }
        }

        if !session.authenticated() {
            bail!("ユーザー {user} の認証に失敗しました");
        }

        Ok(Self { session })
    }

    /// Connect using [`ShogunDesktopSettings`] (key, password, or ssh-agent).
    pub fn from_settings(settings: &ShogunDesktopSettings) -> Result<Self> {
        let ssh = &settings.ssh;
        if ssh.user.is_empty() {
            bail!("SSHユーザー名が未設定です");
        }

        if !ssh.key_path.is_empty() {
            if ssh.password.is_empty() {
                return Self::connect(
                    &ssh.host,
                    ssh.port,
                    &ssh.user,
                    &SshAuth::PrivateKey(ssh.key_path.clone()),
                );
            }
            return Self::connect_with_key_file(
                &ssh.host,
                ssh.port,
                &ssh.user,
                &ssh.key_path,
                Some(ssh.password.as_str()),
            );
        }

        if !ssh.password.is_empty() {
            return Self::connect(
                &ssh.host,
                ssh.port,
                &ssh.user,
                &SshAuth::Password(ssh.password.clone()),
            );
        }

        Self::connect_agent(&ssh.host, ssh.port, &ssh.user)
    }

    fn connect_with_key_file(
        host: &str,
        port: u16,
        user: &str,
        key_path: &str,
        passphrase: Option<&str>,
    ) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let tcp = TcpStream::connect(&addr).map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;
        tcp.set_read_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;

        let mut session = Session::new().context("SSHセッションの作成に失敗しました")?;
        session.set_tcp_stream(tcp);
        session.handshake().map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;

        session
            .userauth_pubkey_file(user, None, Path::new(key_path), passphrase)
            .map_err(|_| anyhow::anyhow!("ユーザー {user} の認証に失敗しました"))?;

        if !session.authenticated() {
            bail!("ユーザー {user} の認証に失敗しました");
        }

        Ok(Self { session })
    }

    fn connect_agent(host: &str, port: u16, user: &str) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let tcp = TcpStream::connect(&addr).map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;
        tcp.set_read_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(CMD_TIMEOUT_SECS)))?;

        let mut session = Session::new().context("SSHセッションの作成に失敗しました")?;
        session.set_tcp_stream(tcp);
        session.handshake().map_err(|_| {
            anyhow::anyhow!("{host}:{port} への接続に失敗しました")
        })?;

        session
            .userauth_agent(user)
            .map_err(|_| anyhow::anyhow!("ユーザー {user} の認証に失敗しました"))?;

        if !session.authenticated() {
            bail!("ユーザー {user} の認証に失敗しました");
        }

        Ok(Self { session })
    }

    /// Verify the session with `echo ok`.
    pub fn is_connected(&mut self) -> bool {
        self.exec("echo ok").is_ok()
    }

    /// Execute a remote command; returns combined stdout and stderr.
    pub fn exec(&mut self, command: &str) -> Result<String> {
        let mut channel = self
            .session
            .channel_session()
            .map_err(|e| anyhow::anyhow!("コマンド実行に失敗しました: {command} ({e})"))?;
        channel
            .exec(command)
            .map_err(|e| anyhow::anyhow!("コマンド実行に失敗しました: {command} ({e})"))?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        channel
            .read_to_string(&mut stdout)
            .context("stdout の読み取りに失敗しました")?;
        channel
            .stderr()
            .read_to_string(&mut stderr)
            .context("stderr の読み取りに失敗しました")?;
        channel
            .wait_close()
            .context("チャネル終了待ちに失敗しました")?;

        let exit = channel.exit_status().unwrap_or(0);
        let mut combined = stdout;
        if !stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        if exit != 0 {
            bail!(
                "コマンド実行に失敗しました: {command} (終了コード {exit})\n{combined}"
            );
        }

        Ok(combined)
    }

    /// Close the SSH session.
    pub fn disconnect(&mut self) {
        let _ = self.session.disconnect(None, "bye", None);
    }
}

impl Drop for SshClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_variants() {
        let _ = SshAuth::PrivateKey("/tmp/key".into());
        let _ = SshAuth::Password("secret".into());
    }
}
