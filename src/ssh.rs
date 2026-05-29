use crate::native_ssh::NativeSshClient;
use crate::settings::{ConnectionBackend, ControlPathType, ShogunDesktopSettings};
use anyhow::{Result, bail};
use std::hash::{Hash, Hasher};
use std::io::Write as IoWrite;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
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
pub struct SystemSshClient {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) key_path: Option<String>,
    pub(crate) password: Option<String>,
    /// Shared across clones; flipped to false on first ControlMaster failure.
    pub(crate) ctrl_enabled: Arc<AtomicBool>,
    pub(crate) control_path_type: ControlPathType,
    pub(crate) proxy_command: String,
    pub(crate) accept_all_host_keys: bool,
}

impl SystemSshClient {
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
            control_path_type: ControlPathType::Socket,
            proxy_command: String::new(),
            accept_all_host_keys: false,
        })
    }

    pub fn from_settings(settings: &ShogunDesktopSettings) -> Result<Self> {
        let ssh = &settings.ssh;
        if ssh.user.is_empty() && ssh.proxy_command.is_empty() {
            bail!("SSHユーザー名が未設定です");
        }
        let key_path = if !ssh.key_path.is_empty() && std::path::Path::new(&ssh.key_path).exists() {
            Some(ssh.key_path.clone())
        } else {
            None
        };
        let password = if !ssh.password.is_empty() {
            Some(ssh.password.clone())
        } else {
            None
        };
        Ok(Self {
            host: ssh.host.clone(),
            port: ssh.port,
            user: ssh.user.clone(),
            key_path,
            password,
            ctrl_enabled: Arc::new(AtomicBool::new(true)),
            control_path_type: ssh.control_path.clone(),
            proxy_command: ssh.proxy_command.clone(),
            accept_all_host_keys: ssh.accept_all_host_keys,
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
            if let Some(ctrl) = self.control_socket_path() {
                let master_alive = std::path::Path::new(&ctrl).exists();
                let askpass = if self.password.is_some() && !master_alive {
                    Some(self.write_askpass_pub(self.password.as_deref().unwrap())?)
                } else {
                    None
                };

                let result = self.run_ssh(command, Some(&ctrl), askpass.as_ref());
                cleanup_askpass(&askpass);

                return match result {
                    Err(ref e) if is_controlmaster_error(e) => {
                        // ControlMaster not supported on this platform — disable and retry.
                        self.ctrl_enabled.store(false, Ordering::Relaxed);
                        self.exec_direct(command)
                    }
                    other => other,
                };
            }
        }

        self.exec_direct(command)
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
                "-o",
                "ControlMaster=auto",
                "-o",
                &format!("ControlPath={ctrl_path}"),
                "-o",
                "ControlPersist=30",
            ]);
        }

        cmd.args(["-o", "ConnectTimeout=10"]);

        if !self.proxy_command.is_empty() {
            cmd.args(["-o", &format!("ProxyCommand={}", self.proxy_command)]);
        }

        if self.accept_all_host_keys {
            cmd.args(["-o", "StrictHostKeyChecking=no"]);
            #[cfg(windows)]
            cmd.args(["-o", "UserKnownHostsFile=NUL"]);
            #[cfg(not(windows))]
            cmd.args(["-o", "UserKnownHostsFile=/dev/null"]);
        }

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

        // Prevent 0xc0000142 (STATUS_DLL_INIT_FAILED) when spawning a console
        // subsystem process from a GUI app with no attached console.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        if self.user.is_empty() {
            cmd.arg(&self.host);
        } else {
            cmd.arg(format!("{}@{}", self.user, self.host));
        }
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

    /// Stable control path derived from host+port+user and settings.
    /// Returns `None` when ControlMaster is disabled (`ControlPathType::None`).
    pub fn control_socket_path(&self) -> Option<String> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.host.hash(&mut h);
        self.port.hash(&mut h);
        self.user.hash(&mut h);
        let key = h.finish() as u32;

        match self.control_path_type {
            ControlPathType::None => None,
            ControlPathType::NamedPipe => {
                #[cfg(windows)]
                {
                    Some(format!(r"\\.\pipe\sg{key:08x}"))
                }
                #[cfg(not(windows))]
                {
                    let tmp = std::env::temp_dir().to_string_lossy().replace('\\', "/");
                    Some(format!("{tmp}/sg{key:08x}"))
                }
            }
            ControlPathType::Socket => {
                let tmp = std::env::temp_dir().to_string_lossy().replace('\\', "/");
                Some(format!("{tmp}/sg{key:08x}"))
            }
        }
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

impl Drop for SystemSshClient {
    fn drop(&mut self) {}
}

/// Unified SSH client — dispatches to system ssh.exe or russh native backend.
#[derive(Clone)]
pub enum SshClient {
    System(SystemSshClient),
    Native(NativeSshClient),
}

impl SshClient {
    pub fn from_settings(settings: &ShogunDesktopSettings) -> Result<Self> {
        match settings.ssh.connection_backend {
            ConnectionBackend::Native => Ok(SshClient::Native(NativeSshClient::new(settings)?)),
            ConnectionBackend::System => {
                Ok(SshClient::System(SystemSshClient::from_settings(settings)?))
            }
        }
    }

    pub fn exec(&self, command: &str) -> Result<String> {
        match self {
            SshClient::System(c) => c.exec(command),
            SshClient::Native(c) => c.exec(command),
        }
    }

    pub fn control_socket_path(&self) -> Option<String> {
        match self {
            SshClient::System(c) => c.control_socket_path(),
            SshClient::Native(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        match self {
            SshClient::System(c) => c.is_connected(),
            SshClient::Native(c) => c.is_connected(),
        }
    }

    /// Upload a local image to `{project_path}/queue/screenshots/` via `scp`.
    /// Reuses ControlMaster when the system backend has an active socket.
    pub fn upload_image(
        &self,
        local_path: &std::path::Path,
        remote_filename: &str,
        project_path: &str,
    ) -> Result<()> {
        match self {
            SshClient::System(c) => c.upload_image(local_path, remote_filename, project_path),
            SshClient::Native(c) => {
                let (host, port, user, key_path) = c.scp_params();
                run_scp(
                    host,
                    port,
                    user,
                    key_path,
                    None,
                    &[],
                    false,
                    local_path,
                    remote_filename,
                    project_path,
                )
            }
        }
    }
}

impl SystemSshClient {
    pub fn upload_image(
        &self,
        local_path: &std::path::Path,
        remote_filename: &str,
        project_path: &str,
    ) -> Result<()> {
        let socket = if self.ctrl_enabled.load(Ordering::Relaxed) {
            self.control_socket_path()
                .filter(|ctrl| std::path::Path::new(ctrl).exists())
        } else {
            None
        };
        let mut extra_opts: Vec<String> = Vec::new();
        if !self.proxy_command.is_empty() {
            extra_opts.push(format!("ProxyCommand={}", self.proxy_command));
        }
        let extra_refs: Vec<&str> = extra_opts.iter().map(String::as_str).collect();
        run_scp(
            &self.host,
            self.port,
            &self.user,
            self.key_path.as_deref(),
            socket.as_deref(),
            &extra_refs,
            self.accept_all_host_keys,
            local_path,
            remote_filename,
            project_path,
        )
    }
}

fn run_scp(
    host: &str,
    port: u16,
    user: &str,
    key_path: Option<&str>,
    control_socket: Option<&str>,
    extra_ssh_opts: &[&str],
    accept_all_host_keys: bool,
    local_path: &std::path::Path,
    remote_filename: &str,
    project_path: &str,
) -> Result<()> {
    let remote_dest = if user.is_empty() {
        format!("{host}:{project_path}/queue/screenshots/{remote_filename}")
    } else {
        format!("{user}@{host}:{project_path}/queue/screenshots/{remote_filename}")
    };

    let mut cmd = Command::new("scp");
    cmd.arg("-P").arg(port.to_string());
    cmd.arg("-o").arg("ConnectTimeout=10");
    cmd.arg("-o").arg("BatchMode=yes");

    if let Some(sock) = control_socket {
        cmd.arg("-o").arg("ControlMaster=auto");
        cmd.arg("-o").arg(format!("ControlPath={sock}"));
    }

    if accept_all_host_keys {
        cmd.arg("-o").arg("StrictHostKeyChecking=no");
        #[cfg(windows)]
        cmd.args(["-o", "UserKnownHostsFile=NUL"]);
        #[cfg(not(windows))]
        cmd.args(["-o", "UserKnownHostsFile=/dev/null"]);
    } else {
        cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
    }

    for opt in extra_ssh_opts {
        cmd.arg("-o").arg(*opt);
    }

    if let Some(key) = key_path.filter(|k| !k.is_empty()) {
        cmd.arg("-i").arg(key);
    }

    cmd.arg(local_path);
    cmd.arg(&remote_dest);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("scp 起動失敗: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "scp 失敗: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
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
        || msg.contains("bad controlpath")
        // Named Pipe ControlPath fails with this on Win32-OpenSSH (socket API mismatch)
        || msg.contains("getsockname")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_variants() {
        let _ = SshAuth::PrivateKey("/tmp/key".into());
        let _ = SshAuth::Password("secret".into());
    }

    use crate::settings::ControlPathType;

    fn test_client(control_path_type: ControlPathType) -> SystemSshClient {
        SystemSshClient {
            host: "example.com".into(),
            port: 22,
            user: "user".into(),
            key_path: None,
            password: None,
            ctrl_enabled: Arc::new(AtomicBool::new(true)),
            control_path_type,
            proxy_command: String::new(),
            accept_all_host_keys: false,
        }
    }

    #[test]
    fn control_socket_path_none() {
        let client = test_client(ControlPathType::None);
        assert!(client.control_socket_path().is_none());
    }

    #[test]
    fn control_socket_path_socket() {
        let client = test_client(ControlPathType::Socket);
        let path = client.control_socket_path().expect("socket path");
        assert!(path.contains("/sg"));
    }

    #[cfg(windows)]
    #[test]
    fn control_socket_path_named_pipe() {
        let client = test_client(ControlPathType::NamedPipe);
        let path = client.control_socket_path().expect("named pipe path");
        assert!(path.starts_with(r"\\.\pipe\sg"));
    }

    #[test]
    fn controlmaster_error_detection() {
        let e = anyhow::anyhow!("ssh: Bad ControlPath /tmp/sg12345678");
        assert!(is_controlmaster_error(&e));
        let e2 = anyhow::anyhow!("Permission denied (publickey)");
        assert!(!is_controlmaster_error(&e2));
    }

    #[test]
    fn proxy_command_respects_connection_backend() {
        let settings = ShogunDesktopSettings {
            ssh: crate::settings::SshSettings {
                user: "u".into(),
                proxy_command: "coder ssh --stdio ws".into(),
                connection_backend: ConnectionBackend::System,
                ..Default::default()
            },
            ..Default::default()
        };
        let client = SshClient::from_settings(&settings).expect("system backend");
        assert!(matches!(client, SshClient::System(_)));
    }

    #[test]
    fn ssh_client_native_control_path_is_none() {
        let settings = ShogunDesktopSettings {
            ssh: crate::settings::SshSettings {
                user: "u".into(),
                connection_backend: ConnectionBackend::Native,
                ..Default::default()
            },
            ..Default::default()
        };
        let client = SshClient::from_settings(&settings).expect("native client");
        assert!(client.control_socket_path().is_none());
    }
}
