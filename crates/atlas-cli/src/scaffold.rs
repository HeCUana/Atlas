//! 脚手架：首次运行时自动生成配置文件与必要目录。
//!
//! ## 为什么需要
//!
//! 一个新克隆的仓库里没有 `atlas.toml`，也没有 `plugins/`、`logs/` 这些目录。
//! 如果不处理，Atlas 会因为「找不到配置文件」而只能跑一个空默认值，
//! 插件发现会静默返回空列表，服务端的 `workdir` 缺失会直接导致进程启动失败。
//!
//! ## 行为
//!
//! - 配置文件不存在 → 自动写入 [`atlas_core::AtlasConfig::template_toml`] 生成的模板；
//! - 配置里引用到的目录不存在 → 自动创建（幂等，已存在则跳过）；
//! - `atlas init` 额外创建服务器根目录 `servers/`，并可用 `--force` 覆盖已有配置。

use std::path::{Path, PathBuf};

use atlas_core::{AtlasConfig, Result};

/// 脚手架的执行结果。
#[derive(Debug, Default)]
pub struct Scaffold {
    /// 本次新建的配置文件路径。
    pub created_config: Option<PathBuf>,
    /// 本次新建的目录。
    pub created_dirs: Vec<PathBuf>,
    /// 本次新建的服务器根目录。
    pub created_servers_dir: Option<PathBuf>,
    /// 本次新建的密钥目录。
    pub created_keys_dir: Option<PathBuf>,
}

impl Scaffold {
    /// 生成人类可读的结果摘要。
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        if let Some(path) = &self.created_config {
            lines.push(format!("已写入配置文件: {}", path.display()));
        }
        if let Some(path) = &self.created_servers_dir {
            lines.push(format!("已创建服务器根目录: {}", path.display()));
        }
        if let Some(path) = &self.created_keys_dir {
            lines.push(format!("已创建密钥目录: {}", path.display()));
        }
        for dir in &self.created_dirs {
            lines.push(format!("已创建目录: {}", dir.display()));
        }
        if lines.is_empty() {
            lines.push("配置与目录均已就绪，无需改动".to_owned());
        }
        lines.join("\n")
    }
}

/// 确保配置文件存在且结构是当前的。
///
/// - 文件不存在 → 写入完整模板，返回 `Some(path)`；
/// - 文件已存在 → **不覆盖用户内容**，只补齐**缺失的段落**
///   （老版本配置里没有 `[sftp_server]`，直接跑 `atlas serve` 会一头雾水）。
///   补齐的段落用**默认值**（`enabled = false`），不会改变既有行为；
/// - 文件已存在且 `force` 为真 → 用模板覆盖，返回 `Some(path)`。
pub fn ensure_config(config_path: &Path, force: bool) -> Result<Option<PathBuf>> {
    if config_path.exists() && !force {
        let text = std::fs::read_to_string(config_path)?;
        if let Some(upgraded) = upgrade_existing(&text) {
            std::fs::write(config_path, upgraded)?;
            return Ok(Some(config_path.to_path_buf()));
        }
        return Ok(None);
    }

    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(config_path, AtlasConfig::template_toml())?;
    Ok(Some(config_path.to_path_buf()))
}

/// 给已有配置补齐缺失的段落，返回 `None` 表示无需改动。
fn upgrade_existing(text: &str) -> Option<String> {
    let mut out = text.to_owned();
    let mut changed = false;

    if !out.contains("[sftp_server]") {
        out.push_str(
            "\n# ── SFTP 服务端（可选）────────────────────────────────────────\n\
             # 启用后，远端电脑可以用 SFTP 连到 Atlas，管理上面这些服务器的目录。\n\
             # 客户端看到的是虚拟路径 /<服务器ID>/...，共享目录自动从 [[servers]] 派生。\n\
             [sftp_server]\n\
             enabled = false\n\
             listen = \"0.0.0.0\"\n\
             port = 2222\n\
             host_key = \"keys/host_ed25519\"\n\
             authorized_keys = \"keys/authorized_keys\"\n",
        );
        changed = true;
    }

    changed.then_some(out)
}

/// 确保配置引用到的所有目录存在，返回本次新建的目录。
pub fn ensure_dirs(config: &AtlasConfig) -> Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    for dir in config.required_dirs() {
        if dir.as_os_str().is_empty() || dir.exists() {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        created.push(dir);
    }
    Ok(created)
}

/// 确保服务器根目录存在。
pub fn ensure_servers_dir() -> Result<Option<PathBuf>> {
    let dir = PathBuf::from(AtlasConfig::DEFAULT_SERVERS_DIR);
    if dir.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(&dir)?;
    Ok(Some(dir))
}

/// 确保密钥目录存在（存放 SSH 客户端私钥）。
pub fn ensure_keys_dir() -> Result<Option<PathBuf>> {
    let dir = PathBuf::from(AtlasConfig::DEFAULT_KEYS_DIR);
    if dir.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(&dir)?;
    Ok(Some(dir))
}

/// 完整脚手架：配置 + 目录。
///
/// `create_base_dirs` 为真时额外创建服务器根目录与密钥目录（`atlas init` 用）。
pub fn scaffold(
    config_path: &Path,
    config: &AtlasConfig,
    force: bool,
    create_base_dirs: bool,
) -> Result<Scaffold> {
    let mut result = Scaffold {
        created_config: ensure_config(config_path, force)?,
        ..Scaffold::default()
    };

    if create_base_dirs {
        result.created_servers_dir = ensure_servers_dir()?;
        result.created_keys_dir = ensure_keys_dir()?;
    }
    result.created_dirs = ensure_dirs(config)?;
    Ok(result)
}
