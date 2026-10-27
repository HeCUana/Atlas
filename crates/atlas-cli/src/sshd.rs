//! 本机 SSH 服务端（sshd）的管理与就绪检查。
//!
//! ## 为什么需要
//!
//! SFTP 是 Atlas 的**客户端**能力——它连出去。但前提是**目标主机上有 SSH 服务在监听**。
//! 连本机时（本机就是被 Atlas 托管的 Minecraft 服务器所在机器），前提就是本机的
//! `sshd` 服务必须装好并在跑。否则连接会被直接拒绝（`ECONNREFUSED` / os error 10061）。
//!
//! 这个模块把"装服务、启服务、查状态"收进 CLI，并额外做一次**就绪检查**：
//! 光看服务在跑不够——私钥没生成、公钥没装到 `authorized_keys`、主机公钥不在
//! `known_hosts` 里，照样连不上。
//!
//! ## 安全约定
//!
//! 启动 SSH 服务会**对外开放 22 端口**，属于安全敏感操作。
//! 因此这里只提供**显式命令**（`atlas sshd start`），绝不在 Atlas 启动时自动开启。
//!
//! 启用公钥免密登录（而不是口令）也是本模块的推荐路径。

use std::path::{Path, PathBuf};
use std::process::Command;

use atlas_core::Result;

/// sshd 的默认端口。
pub const DEFAULT_SSH_PORT: u16 = 22;

/// sshd 动作由 CLI 层的 `SshdAction` 表示；本模块只提供实现。
///
/// sshd 的运行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 服务是否已安装。
    pub installed: bool,
    /// 服务是否正在运行。
    pub running: bool,
    /// 开机自启设置（如 `Automatic` / `enabled`）。
    pub startup: Option<String>,
    /// SSH 端口是否有进程在监听。
    pub port_listening: bool,
    /// Atlas 客户端私钥是否存在。
    pub private_key_present: bool,
    /// Atlas 客户端公钥是否已装进服务端的 `authorized_keys`。
    pub public_key_installed: bool,
    /// 公钥实际被安装到的文件路径（未安装时为 `None`）。
    pub public_key_path: Option<PathBuf>,
    /// `.pub` 文件是否与私钥**不匹配**（说明 `.pub` 已陈旧）。
    pub public_key_file_stale: bool,
    /// `authorized_keys` 里混入了**私钥**——严重问题：既让认证失败，又是凭据泄漏。
    pub authorized_keys_has_private_key: bool,
    /// 本机主机公钥是否已在 `known_hosts` 里。
    pub known_host_present: bool,
    /// 本机 SSH **主机密钥**的 SHA256 指纹。
    ///
    /// 第三方 SFTP 客户端（FileZilla / WinSCP）首次连接会弹"未知主机密钥"
    /// 并显示指纹——把这个指纹给运维核对，才能确认连的是真服务器。
    pub host_key_fingerprint: Option<String>,
}

impl Status {
    /// 是否"装好 + 在跑 + 端口通 + 客户端密钥齐备 + 没有私钥污染"。
    pub fn is_ready(&self) -> bool {
        self.installed
            && self.running
            && self.port_listening
            && self.private_key_present
            && self.public_key_installed
            && !self.authorized_keys_has_private_key
    }
}

/// 执行一条系统命令，返回是否成功与标准输出。
fn run(program: &str, args: &[&str]) -> (bool, String) {
    match Command::new(program).args(args).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            (out.status.success(), text)
        }
        Err(_) => (false, String::new()),
    }
}

/// 查询服务状态。
///
/// `private_key` 是 Atlas 客户端私钥路径（来自配置的 `[sftp] key_path`）。
pub fn status(private_key: &std::path::Path) -> Result<Status> {
    let (installed, running, startup) = service_state();

    let port_listening = is_port_listening(DEFAULT_SSH_PORT);
    let private_key_present = private_key.is_file();

    // 从**私钥**推导公钥，而不是读 `.pub` 文件——`.pub` 可能陈旧，
    // 而从私钥推导出来的永远是当前这把私钥对应的公钥。
    let derived = if private_key_present {
        atlas_sftp::keygen::derive_public_key(private_key).ok()
    } else {
        None
    };
    let public_key_path = derived.as_deref().and_then(find_in_authorized_keys);

    // `.pub` 文件是否与私钥一致。
    let public_key_file_stale = match (&derived, private_key.with_extension("pub")) {
        (Some(derived), pub_path) => std::fs::read_to_string(&pub_path)
            .map(|text| !text.trim().starts_with(derived.trim()))
            .unwrap_or(true),
        _ => false,
    };

    // authorized_keys 里是否混入了私钥。
    let authorized_keys_has_private_key = authorized_keys_candidates().iter().any(|path| {
        std::fs::read_to_string(path)
            .map(|text| atlas_sftp::keygen::contains_private_key(&text))
            .unwrap_or(false)
    });

    let known_host_present = known_host_present();
    let host_key_fingerprint = host_key_fingerprint();

    Ok(Status {
        installed,
        running,
        startup,
        port_listening,
        private_key_present,
        public_key_installed: public_key_path.is_some(),
        public_key_path,
        public_key_file_stale,
        authorized_keys_has_private_key,
        known_host_present,
        host_key_fingerprint,
    })
}

/// 本机 SSH 主机密钥的指纹。
fn host_key_fingerprint() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        candidates.push(PathBuf::from(
            r"C:\ProgramData\ssh\ssh_host_ed25519_key.pub",
        ));
    } else {
        candidates.push(PathBuf::from("/etc/ssh/ssh_host_ed25519_key.pub"));
    }
    for path in candidates {
        if let Ok(fp) = atlas_sftp::keygen::fingerprint_file(&path) {
            return Some(fp);
        }
    }
    None
}

/// 在所有候选 `authorized_keys` 里查找包含该公钥的文件。
fn find_in_authorized_keys(pubkey: &str) -> Option<PathBuf> {
    for path in authorized_keys_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.lines().any(|line| line.trim() == pubkey.trim()) {
            return Some(path);
        }
    }
    None
}

/// 把客户端公钥装进服务端的 `authorized_keys`。
///
/// - 公钥从**私钥**推导，不依赖可能陈旧的 `.pub` 文件；
/// - 目标文件里若混有**私钥**，直接拒绝并报错——不往一个已经泄漏凭据的
///   文件里继续追加内容，那只会掩盖问题；
/// - Windows 上顺手把 ACL 收紧到「Administrators + SYSTEM」（sshd 要求）。
pub fn authorize(private_key: &Path) -> Result<PathBuf> {
    let pubkey = atlas_sftp::keygen::derive_public_key(private_key)?;
    let target = authorized_keys_path();

    if let Ok(existing) = std::fs::read_to_string(&target)
        && atlas_sftp::keygen::contains_private_key(&existing)
    {
        return Err(atlas_core::AtlasError::Other(format!(
            "{} 里混有私钥内容，已拒绝写入。\n\
             请先人工清理该文件（删除私钥行），或改用其它 authorized_keys 路径。\n\
             提示：曾经写入该文件的私钥应视为已泄漏，需要作废并重新生成。",
            target.display()
        )));
    }

    let mut content = std::fs::read_to_string(&target).unwrap_or_default();
    let already = content.lines().any(|line| line.trim() == pubkey.trim());
    if !already {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&pubkey);
        content.push('\n');
        std::fs::write(&target, &content)?;
    }

    harden_authorized_keys_acl(&target);
    Ok(target)
}

/// 收紧 `authorized_keys` 的 ACL（Windows）。
#[cfg(windows)]
fn harden_authorized_keys_acl(path: &Path) {
    for args in [
        vec!["/inheritance:r"],
        vec!["/grant:r", "*S-1-5-32-544:F", "*S-1-5-18:F"],
    ] {
        let _ = std::process::Command::new("icacls")
            .arg(path)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(not(windows))]
fn harden_authorized_keys_acl(_path: &Path) {}

/// 启动服务并设为开机自启。
pub fn start() -> Result<()> {
    if cfg!(windows) {
        // 先确保服务存在。
        if !service_installed() {
            return Err(atlas_core::AtlasError::Other(
                "sshd 服务尚未安装，请先运行 `atlas sshd install`".into(),
            ));
        }
        let (ok, _) = run(
            "powershell",
            &["-NoProfile", "-Command", "Start-Service sshd"],
        );
        if !ok {
            return Err(atlas_core::AtlasError::Other(
                "启动 sshd 失败：需要**管理员权限**的终端，或该服务被其它程序锁定".into(),
            ));
        }
        let _ = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Set-Service sshd -StartupType Automatic",
            ],
        );
    } else {
        let (ok, _) = run("systemctl", &["start", "sshd"]);
        if !ok {
            return Err(atlas_core::AtlasError::Other(
                "启动 sshd 失败：可能需要 root 权限，或服务名不是 sshd（部分发行版是 ssh）".into(),
            ));
        }
        let _ = run("systemctl", &["enable", "sshd"]);
    }
    Ok(())
}

/// 停止服务。
pub fn stop() -> Result<()> {
    if cfg!(windows) {
        let (ok, _) = run(
            "powershell",
            &["-NoProfile", "-Command", "Stop-Service sshd"],
        );
        if !ok {
            return Err(atlas_core::AtlasError::Other(
                "停止 sshd 失败：需要**管理员权限**的终端".into(),
            ));
        }
    } else {
        let (ok, _) = run("systemctl", &["stop", "sshd"]);
        if !ok {
            return Err(atlas_core::AtlasError::Other(
                "停止 sshd 失败：可能需要 root 权限".into(),
            ));
        }
    }
    Ok(())
}

/// 安装 SSH 服务端。
pub fn install() -> Result<()> {
    if cfg!(windows) {
        let (ok, _) = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null",
            ],
        );
        if !ok {
            return Err(atlas_core::AtlasError::Other(
                "安装 OpenSSH Server 失败：需要**管理员权限**的终端，且要能连上 Windows Update"
                    .into(),
            ));
        }
    } else {
        return Err(atlas_core::AtlasError::Other(
            "当前平台请用系统包管理器安装 OpenSSH 服务端，例如：\n\
             \x20 Debian/Ubuntu: apt install openssh-server\n\
             \x20 RHEL/Fedora:   dnf install openssh-server\n\
             \x20 Arch:          pacman -S openssh"
                .into(),
        ));
    }
    Ok(())
}

/// 服务是否已安装。
fn service_installed() -> bool {
    if cfg!(windows) {
        let (ok, _) = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "if (Get-Service sshd -ErrorAction SilentlyContinue) { 'yes' }",
            ],
        );
        ok && !run(
            "powershell",
            &["-NoProfile", "-Command", "(Get-Service sshd).Name"],
        )
        .1
        .is_empty()
    } else {
        run("systemctl", &["list-unit-files", "sshd.service"]).0
            || run("systemctl", &["list-unit-files", "ssh.service"]).0
    }
}

/// 查询 `(已安装, 运行中, 启动类型)`。
fn service_state() -> (bool, bool, Option<String>) {
    if cfg!(windows) {
        let (ok, text) = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "$s = Get-Service sshd -ErrorAction SilentlyContinue; if ($s) { \"$($s.Status)|$($s.StartType)\" }",
            ],
        );
        if !ok || text.is_empty() {
            return (false, false, None);
        }
        let mut parts = text.split('|');
        let status = parts.next().unwrap_or_default().trim();
        let start_type = parts.next().unwrap_or_default().trim();
        (
            true,
            status.eq_ignore_ascii_case("Running"),
            Some(start_type.to_owned()),
        )
    } else {
        let (ok, state) = run("systemctl", &["is-active", "sshd"]);
        let running = ok && state.trim() == "active";
        let (_, enabled) = run("systemctl", &["is-enabled", "sshd"]);
        let startup = enabled.trim().to_owned();
        (
            service_installed(),
            running,
            if startup.is_empty() {
                None
            } else {
                Some(startup)
            },
        )
    }
}

/// 端口上是否有进程在监听。
fn is_port_listening(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(800)).is_ok()
}

/// 服务端可能读取的 `authorized_keys` 候选路径。
///
/// Windows 管理员账户走 `C:\ProgramData\ssh\administrators_authorized_keys`
/// （`sshd_config` 的 `Match Group administrators` 规则），其它账户与
/// Linux 走 `~/.ssh/authorized_keys`。两种都检查，避免漏判。
pub fn authorized_keys_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        out.push(PathBuf::from(
            r"C:\ProgramData\ssh\administrators_authorized_keys",
        ));
    }
    out.push(home_dir().join(".ssh").join("authorized_keys"));
    out
}

/// **推荐**放置公钥的路径（用于提示文案）。
pub fn authorized_keys_path() -> PathBuf {
    if cfg!(windows) && is_admin() {
        PathBuf::from(r"C:\ProgramData\ssh\administrators_authorized_keys")
    } else {
        home_dir().join(".ssh").join("authorized_keys")
    }
}

/// 本机主机公钥是否已在 `known_hosts` 里。
fn known_host_present() -> bool {
    let path = home_dir().join(".ssh").join("known_hosts");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    // known_hosts 里主机名可能是 `127.0.0.1`、`[127.0.0.1]:22`、`localhost` 等写法。
    text.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let host = line.split_whitespace().next().unwrap_or_default();
        host.split(',').any(|h| {
            let h = h.trim();
            h == "127.0.0.1" || h == "localhost" || h == "[127.0.0.1]:22" || h == "[localhost]:22"
        })
    })
}

/// 用户主目录。
fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 当前进程是否是管理员（仅 Windows 有意义）。
fn is_admin() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let (ok, text) = run(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "if (([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { 'yes' }",
        ],
    );
    ok && text.trim() == "yes"
}
