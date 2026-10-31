//! 本机 SSH 服务端（sshd）的管理与就绪检查。
//!
//! ## 为什么需要
//!
//! Atlas 的 SFTP 能力是**服务端**方向——远端电脑用 FileZilla / WinSCP /
//! `sftp` 命令行连进来管 Atlas 托管的服务器文件。但**本机**的这台机器
//! 往往也需要能被第三方 SFTP 工具访问（改配置、捞日志、传文件），
//! 前提就是本机的 `sshd` 服务必须装好并在跑。
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

/// macOS 上 OpenSSH 服务的 launchd **标签**。
///
/// 与 plist 的文件名不是一回事：plist 叫 `ssh.plist`，标签叫 `com.openssh.sshd`。
const MACOS_SSHD_LABEL: &str = "com.openssh.sshd";

/// macOS 上 OpenSSH 服务的 launchd **plist 路径**。
const MACOS_SSHD_PLIST: &str = "/System/Library/LaunchDaemons/ssh.plist";

/// macOS 自带 OpenSSH 服务端，二进制在 `/usr/libexec/` 下。
const MACOS_SSHD_BINARY: &str = "/usr/libexec/sshd-keygen-wrapper";

/// 本机的 SSH 服务管理后端。
///
/// 三个平台的"系统 SSH 服务"完全不是一回事：
///
/// | 平台 | 管理方式 | 服务名 |
/// | --- | --- | --- |
/// | Windows | 服务管理器（PowerShell） | `sshd` |
/// | macOS | launchd | 标签 `com.openssh.sshd`，plist `ssh.plist` |
/// | Linux | systemd | `sshd.service` / `ssh.service` |
///
/// macOS **自带** OpenSSH，用 launchd 管理（系统设置里的「远程登录」开关），
/// **没有** systemd——所以 macOS 上不能拿 `systemctl` 去问。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Windows 服务。
    Windows,
    /// macOS 的 launchd。
    MacOs,
    /// Linux 的 systemd。
    Systemd,
}

/// 当前平台的服务管理后端。
fn backend() -> Backend {
    if cfg!(windows) {
        Backend::Windows
    } else if cfg!(target_os = "macos") {
        Backend::MacOs
    } else {
        Backend::Systemd
    }
}

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
    /// 供第三方客户端使用的私钥是否存在。
    pub private_key_present: bool,
    /// 该私钥对应的公钥是否已装进服务端的 `authorized_keys`。
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
/// `private_key` 是供第三方客户端使用的私钥路径（默认 `keys/id_ed25519`）。
pub fn status(private_key: &std::path::Path) -> Result<Status> {
    // 端口探测先做：macOS 上它就是「服务是否真在收连接」的判据
    // （launchd 持有监听套接字，sshd 进程按需拉起），要喂给服务状态判定。
    let port_listening = is_port_listening(DEFAULT_SSH_PORT);
    let (installed, running, startup) = service_state(port_listening);

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
/// - 目标目录不存在时先建出来（macOS / Linux 上新账号默认没有 `~/.ssh`）；
/// - 写完顺手把 ACL / 权限收紧到 sshd 要求的水平。
pub fn authorize(private_key: &Path) -> Result<PathBuf> {
    let pubkey = atlas_sftp::keygen::derive_public_key(private_key)?;
    let target = authorized_keys_path();
    write_authorized_key(&target, &pubkey)?;
    Ok(target)
}

/// 把一行公钥写进 `target`，目录不存在时先建出来。
fn write_authorized_key(target: &Path, pubkey: &str) -> Result<()> {
    if let Ok(existing) = std::fs::read_to_string(target)
        && atlas_sftp::keygen::contains_private_key(&existing)
    {
        return Err(atlas_core::AtlasError::Other(format!(
            "{} 里混有私钥内容，已拒绝写入。\n\
             请先人工清理该文件（删除私钥行），或改用其它 authorized_keys 路径。\n\
             提示：曾经写入该文件的私钥应视为已泄漏，需要作废并重新生成。",
            target.display()
        )));
    }

    // 目标目录可能还不存在——macOS / Linux 上新账号默认**没有** `~/.ssh`，
    // 不先建出来的话写入会抛一个看不出所以然的英文系统错误。
    ensure_parent_dir(target)?;

    let mut content = std::fs::read_to_string(target).unwrap_or_default();
    let already = content.lines().any(|line| line.trim() == pubkey.trim());
    if !already {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(pubkey);
        content.push('\n');
        std::fs::write(target, &content).map_err(|e| {
            atlas_core::AtlasError::Other(format!("写入 {} 失败：{e}", target.display()))
        })?;
    }

    harden_authorized_keys_permissions(target);
    Ok(())
}

/// 确保 `authorized_keys` 所在目录存在。
///
/// macOS / Linux 上新账号默认**没有** `~/.ssh`，不先建出来的话写入会抛出
/// 一个看不出所以然的系统错误（`No such file or directory`）。
///
/// 目录权限不在这里定——`harden_authorized_keys_permissions` 写完会把父目录
/// 收紧到 `0700`，那才是 sshd 的 StrictModes 真正要求的。
fn ensure_parent_dir(target: &Path) -> Result<()> {
    let Some(dir) = target.parent() else {
        return Ok(());
    };
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| {
        atlas_core::AtlasError::Other(format!(
            "创建目录 {} 失败：{e}\n提示：手动执行 `mkdir -p {}` 后重试。",
            dir.display(),
            dir.display()
        ))
    })
}

/// 收紧 `authorized_keys` 的 ACL（Windows）。
/// 收紧 `authorized_keys` 的权限，让 sshd 真的愿意读它。
///
/// sshd 的 **StrictModes**（默认开）会检查权限：
/// - `authorized_keys` 不能被同机其它用户读写（必须 `0600`）；
/// - `~/.ssh` 目录也不能被同机其它用户写入（必须 `0700`）。
///
/// 权限过宽的话 sshd **不会报错，只会静默忽略**这个文件——表现就是
/// "公钥明明装上了却还是登不进去"，极难排查。
///
/// Windows 上没有 POSIX 权限位，改用 `icacls` 收紧 ACL。
pub fn harden_authorized_keys_permissions(path: &Path) {
    #[cfg(windows)]
    {
        harden_authorized_keys_acl(path);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!("收紧 {} 的权限失败: {e}", path.display());
        }
        // 父目录（通常是 ~/.ssh）也要收紧，否则 sshd 仍会拒绝。
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        {
            tracing::warn!("收紧目录 {} 的权限失败: {e}", dir.display());
        }
    }
}

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

/// 启动服务并设为开机自启。
pub fn start() -> Result<()> {
    match backend() {
        Backend::Windows => {
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
        }
        Backend::MacOs => {
            // macOS 自带 OpenSSH，服务由 launchd 管理。
            // `sudo -n` 不弹密码——非交互环境里没法定向用户要口令，
            // 失败就直接把可照抄的命令给出去。
            let (ok, _) = run("sudo", &["-n", "launchctl", "load", "-w", MACOS_SSHD_PLIST]);
            if !ok {
                return Err(atlas_core::AtlasError::Other(format!(
                    "启动 sshd 失败：需要**管理员权限**。请手动执行：\n\
                     \x20 sudo launchctl load -w {MACOS_SSHD_PLIST}\n\
                     （这等同于系统设置 → 通用 → 共享 → 打开「远程登录」）"
                )));
            }
        }
        Backend::Systemd => {
            let (ok, _) = run("systemctl", &["start", "sshd"]);
            if !ok {
                return Err(atlas_core::AtlasError::Other(
                    "启动 sshd 失败：可能需要 root 权限，或服务名不是 sshd（部分发行版是 ssh）"
                        .into(),
                ));
            }
            let _ = run("systemctl", &["enable", "sshd"]);
        }
    }
    Ok(())
}

/// 停止服务。
pub fn stop() -> Result<()> {
    match backend() {
        Backend::Windows => {
            let (ok, _) = run(
                "powershell",
                &["-NoProfile", "-Command", "Stop-Service sshd"],
            );
            if !ok {
                return Err(atlas_core::AtlasError::Other(
                    "停止 sshd 失败：需要**管理员权限**的终端".into(),
                ));
            }
        }
        Backend::MacOs => {
            let (ok, _) = run(
                "sudo",
                &["-n", "launchctl", "unload", "-w", MACOS_SSHD_PLIST],
            );
            if !ok {
                return Err(atlas_core::AtlasError::Other(format!(
                    "停止 sshd 失败：需要**管理员权限**。请手动执行：\n\
                     \x20 sudo launchctl unload -w {MACOS_SSHD_PLIST}"
                )));
            }
        }
        Backend::Systemd => {
            let (ok, _) = run("systemctl", &["stop", "sshd"]);
            if !ok {
                return Err(atlas_core::AtlasError::Other(
                    "停止 sshd 失败：可能需要 root 权限".into(),
                ));
            }
        }
    }
    Ok(())
}

/// 安装 SSH 服务端。
pub fn install() -> Result<()> {
    match backend() {
        Backend::Windows => {
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
        }
        Backend::MacOs => {
            // macOS **自带** OpenSSH（服务端二进制在 /usr/libexec/），
            // 不需要装任何东西——只要把服务拉起来。
            println!("macOS 自带 OpenSSH 服务端，无需安装。");
            println!("直接运行 `atlas sshd start` 即可（等同于打开「远程登录」）。");
        }
        Backend::Systemd => {
            return Err(atlas_core::AtlasError::Other(
                "当前平台请用系统包管理器安装 OpenSSH 服务端，例如：\n\
                 \x20 Debian/Ubuntu: apt install openssh-server\n\
                 \x20 RHEL/Fedora:   dnf install openssh-server\n\
                 \x20 Arch:          pacman -S openssh"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// 服务是否已安装。
fn service_installed() -> bool {
    match backend() {
        Backend::Windows => {
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
        }
        Backend::MacOs => {
            // macOS 自带 OpenSSH，服务端二进制与 plist 都在系统目录里。
            std::path::Path::new(MACOS_SSHD_PLIST).exists()
                || std::path::Path::new(MACOS_SSHD_BINARY).exists()
        }
        Backend::Systemd => {
            run("systemctl", &["list-unit-files", "sshd.service"]).0
                || run("systemctl", &["list-unit-files", "ssh.service"]).0
        }
    }
}

/// 查询 `(已安装, 运行中, 启动类型)`。
///
/// `port_listening` 是端口探测结果（`is_port_listening` 的结果）。
/// Windows / systemd 的服务状态能直接问出来，这个参数只被 macOS 用到——
/// 但仍然统一传进来，免得每个后端各探一次端口。
fn service_state(port_listening: bool) -> (bool, bool, Option<String>) {
    match backend() {
        Backend::Windows => {
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
        }
        Backend::MacOs => {
            let (installed, running, startup) = macos_service_state(port_listening);
            (installed, running, startup)
        }
        Backend::Systemd => {
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
}

/// macOS 上 OpenSSH 服务的探测结果。
///
/// 单独抽出来是为了让判定逻辑能脱离 macOS 单测——本机是 Windows，
/// `launchctl` 跑不了，但**判定规则**是可以在任何平台验证的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MacOsProbe {
    /// `launchctl print system/com.openssh.sshd` 是否成功。
    ///
    /// 成功即代表服务已加载到 system 域；失败可能是没加载，也可能只是
    /// 当前用户没权限读 system 域——两种情况都不该断言「服务没装」。
    print_ok: bool,
    /// `launchctl print` 输出里有没有 `state = running`。
    launchd_running: bool,
    /// `launchctl print-disabled system` 里该标签是否被标成 `=> true`。
    ///
    /// 它管的是**开机自启**，跟「当前有没有在收连接」是两件事——
    /// 手动拉起来跑但没开开机自启是合法状态，两者正交。
    disabled: bool,
    /// 22 端口上有没有连接在监听。
    port_listening: bool,
}

/// 由探测结果推出 `(运行中, 开机自启)`。
///
/// 两个返回值是**正交**的：`运行中` 说现在有没有在收连接，`开机自启` 说的是
/// 重启机器后会不会自己起来。被禁用只影响后者。
///
/// **为什么不能只看 `state = running`**：macOS 的 sshd 由 launchd 以
/// socket 激活（`inetdCompatibility`）方式托管——launchd 自己持有 22 端口
/// 的监听套接字，收到连接才拉起 `sshd -i`。于是空闲时：
///
/// - launchd 报的是 `state = waiting`，**永远不会是** `running`；
/// - `ps` 里看不到 `sshd` 进程；
/// - 但 22 端口**确实在监听**（`lsof -i :22` 显示的是 `launchd` 而不是 `sshd`）。
///
/// 所以「有没有在收连接」只能看端口，「服务有没有加载」才看 `launchctl print`。
/// 端口探测也不需要管理员权限，是唯一在任意账号下都可靠的信号。
fn macos_decide(p: MacOsProbe) -> (bool, Option<String>) {
    let running = p.port_listening || p.launchd_running;

    let startup = if p.disabled {
        Some("disabled".to_owned())
    } else if p.print_ok || running {
        Some("on".to_owned())
    } else {
        None
    };

    (running, startup)
}

/// macOS 上 OpenSSH 服务的 `(运行中, 开机自启)`。
///
/// launchd 的服务状态不在 systemd 那边：
/// `launchctl print system/<标签>` 能读到即已加载；是否开机自启则看
/// `launchctl print-disabled system` 里该标签有没有被标成 `true`（被禁用）。
fn macos_service_state(port_listening: bool) -> (bool, bool, Option<String>) {
    let installed = service_installed();

    let (print_ok, text) = run(
        "launchctl",
        &["print", &format!("system/{MACOS_SSHD_LABEL}")],
    );
    let launchd_running = print_ok && text.lines().any(|l| l.trim() == "state = running");

    // 开机自启：被「远程登录」关掉时，launchd 会把该服务标成 disabled。
    let (_, disabled) = run("launchctl", &["print-disabled", "system"]);
    let is_disabled = disabled
        .lines()
        .any(|line| line.contains(MACOS_SSHD_LABEL) && line.contains("=> true"));

    let (running, startup) = macos_decide(MacOsProbe {
        print_ok,
        launchd_running,
        disabled: is_disabled,
        port_listening,
    });

    (installed, running, startup)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份探测结果，只改需要的字段。
    fn probe() -> MacOsProbe {
        MacOsProbe {
            print_ok: true,
            launchd_running: false,
            disabled: false,
            port_listening: false,
        }
    }

    #[test]
    fn macos_端口在监听即视为运行中() {
        // macOS 的 sshd 由 launchd socket 激活：空闲时 launchd 报 waiting、
        // ps 里没有 sshd 进程，但 22 端口确实在监听（监听者是 launchd）。
        let p = MacOsProbe {
            port_listening: true,
            ..probe()
        };
        let (running, startup) = macos_decide(p);
        assert!(running, "端口在监听就该算运行中");
        assert_eq!(startup.as_deref(), Some("on"));
    }

    #[test]
    fn macos_空闲时不会误判成未运行() {
        // 回归点：只看 `state = running` 会把「已加载但空闲」的服务判成没跑。
        let p = MacOsProbe {
            print_ok: true,
            launchd_running: false,
            port_listening: true,
            ..probe()
        };
        let (running, _) = macos_decide(p);
        assert!(running);
    }

    #[test]
    fn macos_服务已加载但端口没监听算未运行() {
        // 刚 load 完、或手动 unload 掉 socket 的场景。
        let (running, startup) = macos_decide(probe());
        assert!(!running);
        assert_eq!(
            startup.as_deref(),
            Some("on"),
            "服务已加载仍应报开机自启状态"
        );
    }

    #[test]
    fn macos_launchd报running也算运行中() {
        // 自建 standalone LaunchDaemon（不走 socket 激活）时端口探测可能拿不到。
        let p = MacOsProbe {
            launchd_running: true,
            ..probe()
        };
        let (running, _) = macos_decide(p);
        assert!(running);
    }

    #[test]
    fn macos_被禁用时报disabled() {
        // 「远程登录」关掉后：服务没加载、端口没监听、开机自启被标成 disabled。
        let p = MacOsProbe {
            disabled: true,
            ..probe()
        };
        let (running, startup) = macos_decide(p);
        assert_eq!(startup.as_deref(), Some("disabled"));
        assert!(!running, "没在监听就不该报运行中");
    }

    #[test]
    fn macos_禁用只影响开机自启不影响当前状态() {
        // 两个字段是正交的：`disabled` 说的是**开机自启**，`running` 说的是
        // **当前有没有在收连接**。手动拉起来跑、但没开开机自启是合法状态，
        // 不能因为看到 disabled 就报「服务未运行」。
        let p = MacOsProbe {
            disabled: true,
            port_listening: true,
            ..probe()
        };
        let (running, startup) = macos_decide(p);
        assert!(running, "端口在监听就是真在跑");
        assert_eq!(startup.as_deref(), Some("disabled"));
    }

    #[test]
    fn macos_什么读不到时既不报运行也不报自启() {
        let p = MacOsProbe {
            print_ok: false,
            launchd_running: false,
            disabled: false,
            port_listening: false,
        };
        let (running, startup) = macos_decide(p);
        assert!(!running);
        assert_eq!(startup, None);
    }

    #[test]
    fn macos_无权限读system域时靠端口兜底() {
        // `launchctl print system/...` 读不到，但端口在监听 → 服务肯定在跑。
        let p = MacOsProbe {
            print_ok: false,
            port_listening: true,
            ..probe()
        };
        let (running, startup) = macos_decide(p);
        assert!(running);
        assert_eq!(startup.as_deref(), Some("on"));
    }

    /// 一条合法的公钥（占位内容，格式对就行）。
    const PUBKEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample atlas-test";

    #[test]
    fn 授权写入会自动创建缺失的目录() {
        // macOS / Linux 新账号默认没有 ~/.ssh——不先建出来就是一堆看不懂的英文错误。
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let target = tmp.path().join(".ssh").join("authorized_keys");
        assert!(!target.parent().expect("应有父目录").exists());

        write_authorized_key(&target, PUBKEY).expect("写入不应失败");

        assert!(target.is_file(), "应能创建 authorized_keys");
        let content = std::fs::read_to_string(&target).expect("应能读回");
        assert!(content.contains(PUBKEY));
    }

    #[test]
    fn 授权写入不会丢掉已有公钥() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let target = tmp.path().join(".ssh").join("authorized_keys");
        let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOther other-test";
        std::fs::create_dir_all(target.parent().expect("应有父目录")).expect("应能创建目录");
        std::fs::write(&target, format!("{other}\n")).expect("应能写入");

        write_authorized_key(&target, PUBKEY).expect("写入不应失败");

        let content = std::fs::read_to_string(&target).expect("应能读回");
        assert!(content.contains(other), "已有公钥不能丢");
        assert!(content.contains(PUBKEY));
    }

    #[test]
    fn 授权写入对同一把钥匙是幂等的() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let target = tmp.path().join(".ssh").join("authorized_keys");

        write_authorized_key(&target, PUBKEY).expect("首次写入不应失败");
        write_authorized_key(&target, PUBKEY).expect("二次写入不应失败");

        let content = std::fs::read_to_string(&target).expect("应能读回");
        assert_eq!(
            content.lines().filter(|l| l.trim() == PUBKEY).count(),
            1,
            "同一把公钥不该写两遍"
        );
    }

    #[test]
    fn 授权写入在文件混有私钥时拒绝动手() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let target = tmp.path().join(".ssh").join("authorized_keys");
        std::fs::create_dir_all(target.parent().expect("应有父目录")).expect("应能创建目录");
        let leaked = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
        std::fs::write(&target, leaked).expect("应能写入");

        let err = write_authorized_key(&target, PUBKEY).unwrap_err();
        assert!(
            err.to_string().contains("混有私钥"),
            "错误信息应点明私钥混入，实际：{err}"
        );

        let content = std::fs::read_to_string(&target).expect("应能读回");
        assert_eq!(content, leaked, "拒绝时不能改动文件");
    }
}
