//! 远程与本地路径处理。
//!
//! 本模块只包含纯函数，不做任何 I/O，因此可以在不连接 SSH 服务器的
//! 情况下被完整地单元测试覆盖——这也是本 crate 的测试主体。
//!
//! ## 约定
//!
//! - **远程路径**一律使用 POSIX 风格（`/` 分隔），与 SFTP 协议一致；
//! - **本地路径**使用 [`std::path::Path`]。映射时按 `/` 与 `\` 两种分隔符切分，
//!   以免 Windows 上的 `..\` 绕过检查。
//!
//! ## 安全保证
//!
//! - [`join_remote`] 把传入的相对路径限制在根目录之内，`..` 无法越过根目录；
//! - [`sanitize_local_path`] **拒绝**任何 `..` 片段，阻断本地路径穿越。

use std::path::{Path, PathBuf};

use atlas_core::{AtlasError, Result};

/// 远程路径分隔符（SFTP 固定使用 POSIX 风格）。
const REMOTE_SEP: char = '/';

/// 路径片段 `.`（当前目录）。
const CURRENT_DIR: &str = ".";

/// 路径片段 `..`（上级目录）。
const PARENT_DIR: &str = "..";

/// 把远程路径切成片段，丢弃空片段与 `.`。
///
/// `..` 片段保留，交由调用方决定如何处理（回退或报错）。
fn segments(path: &str) -> Vec<&str> {
    path.split(REMOTE_SEP)
        .filter(|seg| !seg.is_empty() && *seg != CURRENT_DIR)
        .collect()
}

/// 规整远程路径。
///
/// - `.` 片段被丢弃；
/// - `..` 片段回退一级；
/// - 连续的 `/` 折叠为一个；
/// - 绝对路径（以 `/` 开头）保持绝对形式，相对路径保持相对形式；
/// - 结果为空时，绝对路径归一为 `/`，相对路径归一为 `.`。
///
/// # 错误
///
/// `..` 会越过根目录时返回 [`AtlasError::Sftp`]，而不是静默截断——
/// 静默截断会把 `../../etc/passwd` 变成一个看起来合法的路径。
///
/// # 示例
///
/// ```
/// use atlas_sftp::path::normalize_remote;
///
/// assert_eq!(normalize_remote("a/./b").unwrap(), "a/b");
/// assert_eq!(normalize_remote("a/b/../c").unwrap(), "a/c");
/// assert!(normalize_remote("../etc").is_err());
/// ```
pub fn normalize_remote(path: &str) -> Result<String> {
    let absolute = path.starts_with(REMOTE_SEP);
    let mut stack: Vec<&str> = Vec::new();

    for seg in path.split(REMOTE_SEP) {
        match seg {
            "" | CURRENT_DIR => {}
            PARENT_DIR => {
                if stack.pop().is_none() {
                    return Err(AtlasError::Sftp(format!("远程路径 {path} 越出根目录")));
                }
            }
            other => stack.push(other),
        }
    }

    let joined = stack.join("/");
    if absolute {
        Ok(format!("/{joined}"))
    } else if joined.is_empty() {
        Ok(CURRENT_DIR.to_string())
    } else {
        Ok(joined)
    }
}

/// 拼接远程路径。
///
/// - `base` 是远程根目录，可以是空串、`/` 或普通目录（允许结尾带 `/`）；
/// - `rel` 是相对于 `base` 的路径。**`rel` 的前导 `/` 会被忽略**，
///   也就是 `rel` 永远被解释为 `base` 之下的相对路径（chroot 语义），
///   这样 `/etc/passwd` 之类的输入无法跳出根目录；
/// - `.` 片段被丢弃，`..` 片段回退一级。
///
/// # 错误
///
/// `..` 会越过 `base` 时返回 [`AtlasError::Sftp`]。
///
/// # 示例
///
/// ```
/// use atlas_sftp::path::join_remote;
///
/// assert_eq!(join_remote("/srv/mc", "plugins").unwrap(), "/srv/mc/plugins");
/// assert_eq!(join_remote("/srv/mc/", "/plugins").unwrap(), "/srv/mc/plugins");
/// assert!(join_remote("/srv/mc", "../../etc").is_err());
/// ```
pub fn join_remote(base: &str, rel: &str) -> Result<String> {
    let base_norm = normalize_remote(base)?;
    let base_absolute = base_norm.starts_with(REMOTE_SEP);
    let base_len = segments(&base_norm).len();
    let mut stack = segments(&base_norm);

    for seg in rel.split(REMOTE_SEP) {
        match seg {
            "" | CURRENT_DIR => {}
            PARENT_DIR => {
                if stack.len() <= base_len {
                    return Err(AtlasError::Sftp(format!(
                        "远程路径 {rel} 越出根目录 {base}"
                    )));
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }

    let joined = stack.join("/");
    if base_absolute {
        Ok(format!("/{joined}"))
    } else if joined.is_empty() {
        Ok(CURRENT_DIR.to_string())
    } else {
        Ok(joined)
    }
}

/// 把远程相对路径映射到本地目录，并阻止路径穿越。
///
/// - `rel` 必须是相对路径：以 `/` 或 `\` 开头一律拒绝；
/// - `rel` 中出现 `..` 片段一律拒绝（不解析、不截断）；
/// - `rel` 中出现 `:` 的片段一律拒绝（Windows 盘符与 NTFS 数据流）；
/// - 结果必须仍在 `root` 之下，否则报错。
///
/// `rel` 为空串时返回 `root` 本身。
///
/// # 错误
///
/// `rel` 非法或结果越出 `root` 时返回 [`AtlasError::Sftp`]。
///
/// # 示例
///
/// ```
/// use std::path::Path;
/// use atlas_sftp::path::sanitize_local_path;
///
/// let root = Path::new("/var/lib/atlas");
/// assert_eq!(
///     sanitize_local_path(root, "plugins/config.yml").unwrap(),
///     Path::new("/var/lib/atlas/plugins/config.yml")
/// );
/// assert!(sanitize_local_path(root, "../../etc/passwd").is_err());
/// ```
pub fn sanitize_local_path(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }

    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(AtlasError::Sftp(format!("本地路径 {rel} 必须是相对路径")));
    }

    let mut out = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | CURRENT_DIR => {}
            PARENT_DIR => {
                return Err(AtlasError::Sftp(format!(
                    "本地路径 {rel} 含越界片段 {PARENT_DIR}"
                )));
            }
            other if other.contains(':') => {
                return Err(AtlasError::Sftp(format!(
                    "本地路径片段 {other} 含非法字符 ':'"
                )));
            }
            other => out.push(other),
        }
    }

    // 兜底校验：拼接结果必须仍在 root 之下。
    if !out.starts_with(root) {
        return Err(AtlasError::Sftp(format!(
            "本地路径 {rel} 越出根目录 {}",
            root.display()
        )));
    }

    Ok(out)
}

/// 判断文件名是否为隐藏文件。
///
/// Unix 约定：以 `.` 开头即为隐藏文件。
///
/// 注意：`.` 与 `..` 本身也满足该判定，但它们是目录遍历中的特殊条目，
/// 通常在列举目录时就被过滤掉了。
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// 人类可读的文件大小。
///
/// 以 1024 为基数，输出 `B` / `KB` / `MB` / `GB` / `TB`。
/// 小于 1 KiB 时输出整数字节数，其余保留一位小数。
///
/// # 示例
///
/// ```
/// use atlas_sftp::path::human_size;
///
/// assert_eq!(human_size(512), "512 B");
/// assert_eq!(human_size(1536), "1.5 KB");
/// ```
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;

    let value = bytes as f64;
    if value < KB {
        return format!("{bytes} B");
    }

    let mut value = value / KB;
    let mut unit = "KB";
    if value >= KB {
        value /= KB;
        unit = "MB";
        if value >= KB {
            value /= KB;
            unit = "GB";
            if value >= KB {
                value /= KB;
                unit = "TB";
            }
        }
    }

    format!("{value:.1} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_appends_relative_path() {
        assert_eq!(
            join_remote("/srv/mc", "plugins").unwrap(),
            "/srv/mc/plugins"
        );
        assert_eq!(
            join_remote("/srv/mc", "plugins/config.yml").unwrap(),
            "/srv/mc/plugins/config.yml"
        );
    }

    #[test]
    fn join_remote_tolerates_trailing_and_leading_slash() {
        assert_eq!(
            join_remote("/srv/mc/", "plugins").unwrap(),
            "/srv/mc/plugins"
        );
        assert_eq!(
            join_remote("/srv/mc//", "plugins").unwrap(),
            "/srv/mc/plugins"
        );
        // 前导 `/` 视为相对路径（chroot 语义），不会跳出根目录。
        assert_eq!(
            join_remote("/srv/mc", "/plugins").unwrap(),
            "/srv/mc/plugins"
        );
    }

    #[test]
    fn join_remote_handles_empty_relative() {
        assert_eq!(join_remote("/srv/mc", "").unwrap(), "/srv/mc");
        assert_eq!(join_remote("/", "").unwrap(), "/");
        assert_eq!(join_remote("", "").unwrap(), ".");
    }

    #[test]
    fn join_remote_resolves_dot_and_parent_segments() {
        assert_eq!(
            join_remote("/srv/mc", "./plugins/./config").unwrap(),
            "/srv/mc/plugins/config"
        );
        assert_eq!(
            join_remote("/srv/mc", "plugins/../logs").unwrap(),
            "/srv/mc/logs"
        );
    }

    #[test]
    fn join_remote_rejects_escaping_parent() {
        assert!(join_remote("/srv/mc", "../etc").is_err());
        assert!(join_remote("/srv/mc", "../../etc/passwd").is_err());
        assert!(join_remote("/srv/mc", "a/../../b").is_err());
    }

    #[test]
    fn normalize_remote_canonicalizes() {
        assert_eq!(normalize_remote("a/./b").unwrap(), "a/b");
        assert_eq!(normalize_remote("a/b/../c").unwrap(), "a/c");
        assert_eq!(normalize_remote("./a").unwrap(), "a");
        assert_eq!(normalize_remote("a//b").unwrap(), "a/b");
    }

    #[test]
    fn normalize_remote_keeps_absolute_form() {
        assert_eq!(normalize_remote("/").unwrap(), "/");
        assert_eq!(normalize_remote("/a/b/").unwrap(), "/a/b");
        assert_eq!(normalize_remote("/a/../b").unwrap(), "/b");
        assert_eq!(normalize_remote("").unwrap(), ".");
    }

    #[test]
    fn normalize_remote_rejects_escape() {
        assert!(normalize_remote("../etc").is_err());
        assert!(normalize_remote("/../etc").is_err());
        assert!(normalize_remote("a/../../b").is_err());
    }

    #[test]
    fn sanitize_local_path_maps_nested_relative_path() {
        let root = Path::new("/var/lib/atlas");
        assert_eq!(
            sanitize_local_path(root, "plugins/config.yml").unwrap(),
            Path::new("/var/lib/atlas/plugins/config.yml")
        );
        assert_eq!(sanitize_local_path(root, "").unwrap(), root);
        assert_eq!(
            sanitize_local_path(root, "./plugins").unwrap(),
            Path::new("/var/lib/atlas/plugins")
        );
    }

    #[test]
    fn sanitize_local_path_blocks_traversal() {
        let root = Path::new("/var/lib/atlas");
        assert!(sanitize_local_path(root, "../../etc/passwd").is_err());
        assert!(sanitize_local_path(root, "plugins/../../etc").is_err());
        assert!(sanitize_local_path(root, "..").is_err());
        // Windows 风格分隔符同样被拦截。
        assert!(sanitize_local_path(root, "..\\..\\windows\\system32").is_err());
    }

    #[test]
    fn sanitize_local_path_rejects_absolute_and_drive_paths() {
        let root = Path::new("/var/lib/atlas");
        assert!(sanitize_local_path(root, "/etc/passwd").is_err());
        assert!(sanitize_local_path(root, "\\windows\\system32").is_err());
        assert!(sanitize_local_path(root, "C:\\windows\\system32").is_err());
    }

    #[test]
    fn is_hidden_detects_dot_files() {
        assert!(is_hidden(".env"));
        assert!(is_hidden(".gitignore"));
        assert!(is_hidden("."));
        assert!(!is_hidden("server.jar"));
        assert!(!is_hidden(""));
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(human_size(1024u64.pow(4)), "1.0 TB");
    }
}
