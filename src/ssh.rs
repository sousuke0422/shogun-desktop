use crate::settings::ShogunDesktopSettings;
use anyhow::{bail, Result};
use std::hash::{Hash, Hasher};
use std::io::Write as IoWrite;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// SSH authentication method (legacy API; `from_settings` is preferred).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SshAuth {
    /// Path to a private key file.
    PrivateKey(String),
    /// Password authentication.
    Password(String),
}

/// SSH client backed by the system ssh with optional ControlMaster multiplexing.
///
/// ControlMaster is attempted on first exec(); if the platform doesn't support
/// it (Win32-OpenSSH issue #405) the flag is atomically cleared and all
/// subsequent calls fall back to direct connections transparently.
#[derive(Debug, Clone)]
pub struct SshClient {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) key_path: Option<String>,
    pub(crate) password: Option<String>,
    /// Shared across clones; flipped to false on first ControlMaster failure.
    pub(crate) ctrl_enabled: Arc<AtomicBool>,
}

impl SshClient {
    #[allow(dead_code)]
    pub fn connect(host: &str, port: u16, user: &str, key_or_pass: &SshAuth) -> Result<Self> {
        let (key_path, password) = match key_or_pass {
            SshAuth::PrivateKey(path) => (Some(path.clone()), None),
            SshAuth::Password(pass) => (None, Some(pass.clone())),
        };
        Ok(Self {
            host: host.to_string(),
            port,
            user: user.to_string(),
            key_path,
            password,
            ctrl_enabled: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn from_settings(settings: &ShogunDesktopSettings) -> Result<Self> {
        let ssh = &settings.ssh;
        if ssh.user.is_empty() {
            bail!("SSHユーザー名が未設定です");
        }
        let key_path = if !ssh.key_path.is_empty()
            && std::path::Path::new(&ssh.key_path).exists()
        {
            Some(ssh.key_path.clone())
        } else {
            None
        };
        let password = if !ssh.password.is_empty() { Some(ssh.password.clone()) } else { None };
        Ok(Self {
            host: ssh.host.clone(),
            port: ssh.port,
            user: ssh.user.clone(),
            key_path,
            password,
            ctrl_enabled: Arc::new(AtomicBool::new(true)),
        })
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.exec("echo ok").is_ok()
    }

    /// Execute a remote command; returns combined stdout+stderr.
    ///
    /// Tries ControlMaster first. On ControlMaster-related failure the flag is
    /// disabled for this client instance and the call is retried directly — so
    /// the caller never needs to care about platform support.
    pub fn exec(&self, command: &str) -> Result<String> {
        let use_ctrl = self.ctrl_enabled.load(Ordering::Relaxed);

        if use_ctrl {
            let ctrl = self.control_socket_path();
            let master_alive = std::path::Path::new(&ctrl).exists();
            let askpass = if self.password.is_some() && !master_alive {
                Some(self.write_askpass_pub(self.password.as_deref().unwrap())?)
            } else {
                None
            };

            let result = self.run_ssh(command, Some(&ctrl), askpass.as_ref());
            cleanup_askpass(&askpass);

            match result {
                Err(ref e) if is_controlmaster_error(e) => {
                    // ControlMaster not supported on this platform — disable and retry.
                    self.ctrl_enabled.store(false, Ordering::Relaxed);
                    self.exec_direct(command)
                }
                other => other,
            }
        } else {
            self.exec_direct(command)
        }
    }

    /// Direct connection (no ControlMaster).
    fn exec_direct(&self, command: &str) -> Result<String> {
        let askpass = if self.password.is_some() {
            Some(self.write_askpass_pub(self.password.as_deref().unwrap())?)
        } else {
            None
        };
        let result = self.run_ssh(command, None, askpass.as_ref());
        cleanup_askpass(&askpass);
        result
    }

    fn run_ssh(
        &self,
        command: &str,
        ctrl: Option<&str>,
        askpass: Option<&(std::path::PathBuf, std::path::PathBuf)>,
    ) -> Result<String> {
        let mut cmd = Command::new("ssh");
        cmd.args(["-p", &self.port.to_string()]);

        if let Some(ctrl_path) = ctrl {
            cmd.args([
                "-o", "ControlMaster=auto",
                "-o", &format!("ControlPath={ctrl_path}"),
                "-o", "ControlPersist=30",
            ]);
        }

        cmd.args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "ConnectTimeout=10",
        ]);

        if let Some(ref key) = self.key_path {
            cmd.args(["-i", key]);
        }

        if let Some((bat_path, _)) = askpass {
            cmd.env("SSH_ASKPASS", bat_path);
            cmd.env("SSH_ASKPASS_REQUIRE", "force");
        } else if self.password.is_none() {
            cmd.args(["-o", "BatchMode=yes"]);
        }

        cmd.stdin(Stdio::null());
        cmd.arg(format!("{}@{}", self.user, self.host));
        cmd.arg(command);

        let output = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("ssh の起動に失敗しました: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let mut combined = stdout;
        if !stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!(
                "コマンド実行に失敗しました: {command} (終了コード {code})\n{}",
                combined.trim()
            );
        }

        Ok(combined)
    }

    /// Stable socket path derived from host+port+user.
    /// Forward slashes: Windows OpenSSH parses ControlPath with them.
    pub fn control_socket_path(&self) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.host.hash(&mut h);
        self.port.hash(&mut h);
        self.user.hash(&mut h);
        let key = h.finish() as u32;
        let tmp = std::env::temp_dir().to_string_lossy().replace('\\', "/");
        format!("{tmp}/sg{key:08x}")
    }

    /// Write password to a temp file and create a platform-appropriate askpass wrapper.
    pub(crate) fn write_askpass_pub(
        &self,
        password: &str,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
        let pid = std::process::id();
        let tmp = std::env::temp_dir();
        let pass_path = tmp.join(format!("shogun_p{pid}.tmp"));

        {
            let mut f = std::fs::File::create(&pass_path)
                .map_err(|e| anyhow::anyhow!("パスワードファイル作成失敗: {e}"))?;
            writeln!(f, "{}", password)?;
        }

        #[cfg(windows)]
        let script_path = {
            let bat_path = tmp.join(format!("shogun_a{pid}.bat"));
            {
                let mut f = std::fs::File::create(&bat_path)
                    .map_err(|e| anyhow::anyhow!("askpassスクリプト作成失敗: {e}"))?;
                writeln!(f, "@echo off")?;
                writeln!(f, "type \"{}\"", pass_path.display())?;
            }
            bat_path
        };

        #[cfg(unix)]
        let script_path = {
            use std::os::unix::fs::PermissionsExt;
            let sh_path = tmp.join(format!("shogun_a{pid}.sh"));
            {
                let mut f = std::fs::File::create(&sh_path)
                    .map_err(|e| anyhow::anyhow!("askpassスクリプト作成失敗: {e}"))?;
                writeln!(f, "#!/bin/sh")?;
                writeln!(f, "cat \"{}\"", pass_path.display())?;
            }
            std::fs::set_permissions(&sh_path, std::fs::Permissions::from_mode(0o700))?;
            sh_path
        };

        Ok((script_path, pass_path))
    }

    #[allow(dead_code)]
    pub fn disconnect(&mut self) {}
}

impl Drop for SshClient {
    fn drop(&mut self) {}
}

fn cleanup_askpass(files: &Option<(std::path::PathBuf, std::path::PathBuf)>) {
    if let Some((script, pass)) = files {
        let _ = std::fs::remove_file(script);
        let _ = std::fs::remove_file(pass);
    }
}

/// Detect ControlMaster-related errors (Win32-OpenSSH issue #405 and variants).
fn is_controlmaster_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("controlsocket")
        || msg.contains("controlpath")
        || msg.contains("mux_")
        || msg.contains("multiplexing")
        || msg.contains("control socket")
        // Win32-OpenSSH sometimes emits this when socket creation fails
        || msg.contains("bad controlpath")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_variants() {
        let _ = SshAuth::PrivateKey("/tmp/key".into());
        let _ = SshAuth::Password("secret".into());
    }

    #[test]
    fn controlmaster_error_detection() {
        let e = anyhow::anyhow!("ssh: Bad ControlPath /tmp/sg12345678");
        assert!(is_controlmaster_error(&e));
        let e2 = anyhow::anyhow!("Permission denied (publickey)");
        assert!(!is_controlmaster_error(&e2));
    }
}
