//! 配置文件模型。
//!
//! Atlas 使用 TOML 配置文件。核心配置项：
//!
//! - `[servers.*]`：被管理的 Minecraft 服务端；
//! - `data_source`：数据源模式，可在 **Mod 获取接口** 与 **终端日志抓取** 之间切换；
//! - `[sftp]`：远程文件管理连接参数；
//! - `event_capacity`：事件总线容量。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AtlasError, Result};

/// 单台被管理服务端的配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 服务器 ID，全局唯一。
    pub id: String,
    /// 显示名。
    #[serde(default)]
    pub name: String,
    /// 工作目录（服务端 jar 所在目录）。
    pub workdir: PathBuf,
    /// 启动命令（可执行文件名或相对路径）。
    pub command: String,
    /// 启动参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 附加环境变量。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 额外的 JVM 参数。
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// 是否随 Atlas 启动。
    #[serde(default = "default_true")]
    pub auto_start: bool,
    /// 日志文件路径；`None` 表示走标准输出抓取。
    #[serde(default)]
    pub log_path: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

impl ServerConfig {
    /// 显示名，未设置时回退为 ID。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// 数据源模式。
///
/// 通过配置切换「Mod 获取接口」与「终端日志抓取」两种数据来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DataSource {
    /// 通过 Mod 在游戏中获取信息并传输到 Atlas。
    ///
    /// 需要配置 IP 等网络信息，Mod 侧据此回连。
    Mod {
        /// Atlas 监听的地址。
        host: String,
        /// Atlas 监听的端口。
        port: u16,
        /// 每台服务器的鉴权令牌。
        #[serde(default)]
        token: Option<String>,
    },
    /// 抓取终端日志。
    ///
    /// 通过相对路径或绝对路径控制服务器控制台。相对路径以
    /// Atlas 进程的工作目录为基准解析。
    Log {
        /// 日志路径。相对路径以 Atlas 进程工作目录为基准。
        path: PathBuf,
    },
}

impl Default for DataSource {
    fn default() -> Self {
        Self::Log {
            path: PathBuf::from("logs"),
        }
    }
}

/// SFTP 连接配置。
///
/// ## 敏感字段
///
/// `password` 是敏感字段。**不要**为此类型派生 `Debug`——
/// `Debug` 实现是手写的，口令一律渲染为 `***`，私钥路径保留（路径本身不是机密，
/// 但若不希望暴露请在调用侧自行处理）。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpConfig {
    /// 主机地址。
    pub host: String,
    /// 端口。
    #[serde(default = "default_sftp_port")]
    pub port: u16,
    /// 用户名。
    pub username: String,
    /// 口令（明文存储仅建议用于本地开发）。
    #[serde(default)]
    pub password: Option<String>,
    /// 私钥路径。
    #[serde(default)]
    pub key_path: Option<PathBuf>,
    /// 私钥口令（私钥被加密时才需要）。
    #[serde(default)]
    pub key_passphrase: Option<String>,
    /// 远程根目录（SFTP chroot 之外的起始目录）。
    #[serde(default)]
    pub root: Option<String>,
}

impl std::fmt::Debug for SftpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_ref().map(|_| "***".to_owned()),
            )
            .field("key_path", &self.key_path)
            .field(
                "key_passphrase",
                &self.key_passphrase.as_ref().map(|_| "***".to_owned()),
            )
            .field("root", &self.root)
            .finish()
    }
}

fn default_sftp_port() -> u16 {
    22
}

/// Atlas 全局配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasConfig {
    /// 被管理的服务器列表。
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
    /// 数据源模式。
    #[serde(default)]
    pub data_source: DataSource,
    /// SFTP 连接配置。
    #[serde(default)]
    pub sftp: Option<SftpConfig>,
    /// 事件总线容量。
    #[serde(default = "default_event_capacity")]
    pub event_capacity: usize,
    /// 插件目录。
    #[serde(default = "default_plugin_dir")]
    pub plugin_dir: PathBuf,
    /// 日志解析 profile 覆盖文件（可选）。
    #[serde(default)]
    pub log_profiles: Option<PathBuf>,
}

fn default_event_capacity() -> usize {
    crate::event::DEFAULT_EVENT_CAPACITY
}

fn default_plugin_dir() -> PathBuf {
    PathBuf::from("plugins")
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            data_source: DataSource::default(),
            sftp: None,
            event_capacity: default_event_capacity(),
            plugin_dir: default_plugin_dir(),
            log_profiles: None,
        }
    }
}

impl AtlasConfig {
    /// 从 TOML 文件加载配置。
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AtlasError::config(format!("无法读取配置文件 {}: {e}", path.display())))?;
        let config: Self =
            toml::from_str(&text).map_err(|e| AtlasError::config(format!("配置解析失败: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// 校验配置合法性。
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for server in &self.servers {
            if server.id.trim().is_empty() {
                return Err(AtlasError::config("服务器 id 不能为空"));
            }
            if !seen.insert(server.id.as_str()) {
                return Err(AtlasError::config(format!("服务器 id 重复: {}", server.id)));
            }
            if server.command.trim().is_empty() {
                return Err(AtlasError::config(format!(
                    "服务器 {} 的 command 不能为空",
                    server.id
                )));
            }
        }
        Ok(())
    }

    /// 按 ID 查找服务器配置。
    pub fn server(&self, id: &str) -> Option<&ServerConfig> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// 所有服务器 ID。
    pub fn server_ids(&self) -> Vec<crate::server::ServerId> {
        self.servers
            .iter()
            .map(|s| crate::server::ServerId::new(s.id.clone()))
            .collect()
    }

    /// 脚手架需要创建的**服务器根目录**。
    ///
    /// 新建服务器时把 `workdir` 放在这个目录下比较省心。
    pub const DEFAULT_SERVERS_DIR: &'static str = "servers";

    /// 默认的**密钥目录**（存放 SSH 客户端私钥）。
    pub const DEFAULT_KEYS_DIR: &'static str = "keys";

    /// 默认的 **SSH 客户端私钥路径**（相对工作目录）。
    pub const DEFAULT_KEY_PATH: &'static str = "keys/id_ed25519";

    /// 默认的 **PKCS#8 PEM 私钥路径**（相对工作目录）。
    pub const DEFAULT_KEY_PATH_PKCS8: &'static str = "keys/id_ed25519.pem";

    /// 返回运行期**必须存在**的目录。
    ///
    /// 包含：
    /// - 插件目录（[`Self::plugin_dir`]）；
    /// - 日志目录（`data_source` 为 `Log` 时的 `path`；
    ///   若该路径看起来是**文件**（有扩展名）则取其父目录，否则当作目录本身）；
    /// - 每台服务器的 `workdir`。
    ///
    /// 目录不存在时应当被创建——服务端的 `workdir` 缺失会导致进程启动失败，
    /// 而插件目录缺失会让插件发现静默返回空列表。
    pub fn required_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();

        if !self.plugin_dir.as_os_str().is_empty() {
            dirs.push(self.plugin_dir.clone());
        }

        if let DataSource::Log { path } = &self.data_source
            && !path.as_os_str().is_empty()
        {
            // 带扩展名 → 视为日志文件，取其父目录；否则视为目录本身。
            let dir = if path.extension().is_some() {
                path.parent().map(Path::to_path_buf).unwrap_or_default()
            } else {
                path.clone()
            };
            if !dir.as_os_str().is_empty() {
                dirs.push(dir);
            }
        }

        for server in &self.servers {
            if !server.workdir.as_os_str().is_empty() {
                dirs.push(server.workdir.clone());
            }
        }

        // SFTP 客户端私钥所在目录：私钥不存在时要报错，目录得先在。
        if let Some(sftp) = &self.sftp
            && let Some(key_path) = &sftp.key_path
            && let Some(parent) = key_path.parent()
            && !parent.as_os_str().is_empty()
        {
            dirs.push(parent.to_path_buf());
        }

        dirs
    }

    /// 生成一份可用的配置模板（TOML 文本）。
    ///
    /// 模板本身是**合法的 TOML**，可以直接被 [`Self::load_from`] 读回；
    /// 服务器段落以注释形式给出，取消注释即可启用。
    pub fn template_toml() -> String {
        let mut out = String::new();
        out.push_str(
            "# Atlas 配置文件\n\
             #\n\
             # 本文件由 `atlas init` 自动生成。\n\
             # 相对路径一律以 Atlas 进程的工作目录为基准解析。\n\
             \n\
             # ── 被托管的 Minecraft 服务端 ─────────────────────────────────\n\
             # 取消注释并按需修改即可添加一台服务器。\n\
             #\n\
             # [[servers]]\n\
             # id = \"survival\"                  # 全局唯一\n\
             # name = \"生存服\"                   # 显示名\n\
             # workdir = \"servers/survival\"      # 服务端 jar 所在目录（不存在会自动创建）\n\
             # command = \"java\"                  # 启动命令\n\
             # args = [\"-jar\", \"server.jar\", \"nogui\"]\n\
             # jvm_args = [\"-Xms2G\", \"-Xmx2G\"]   # 会拼在 args 之前\n\
             # auto_start = true                  # 是否随 Atlas 启动\n\
             #\n\
             # # 附加环境变量\n\
             # [servers.env]\n\
             # JAVA_TOOL_OPTIONS = \"\"\n\
             \n\
             # ── 数据源模式 ─────────────────────────────────────────────\n\
             # mode = \"log\"  终端日志抓取（需要 path）\n\
             # mode = \"mod\"  Mod 获取接口（需要 host / port）\n\
             [data_source]\n\
             mode = \"log\"\n\
             path = \"logs\"\n\
             \n\
             # ── 事件总线容量 ─────────────────────────────────────────────\n\
             # 广播通道大小；满了会淘汰最旧事件，落后消费者会收到 Lagged 信号。\n\
             event_capacity = 1024\n\
             \n\
             # ── 插件目录 ─────────────────────────────────────────────────\n\
             # 支持单文件插件、`.atlas` 打包插件、`.rs` 源码插件与文件夹插件。\n\
             plugin_dir = \"plugins\"\n\
             \n\
             # ── 日志解析 profile 覆盖（可选） ─────────────────────────────\n\
             # 覆盖内置的 Log4j2 / CraftBukkit 解析规则。\n\
             # log_profiles = \"log-profiles.toml\"\n\
             \n\
             # ── SFTP 远程文件管理（可选） ────────────────────────────────\n\
             # 客户端私钥用 `atlas keygen` 生成（默认落在 keys/id_ed25519）。\n\
             # [sftp]\n\
             # host = \"127.0.0.1\"\n\
             # port = 22\n\
             # username = \"atlas\"\n\
             # key_path = \"keys/id_ed25519\"\n\
             # key_passphrase = \"\"     # 私钥被加密时才需要\n\
             # root = \"/home/mc\"\n",
        );
        out
    }
}
