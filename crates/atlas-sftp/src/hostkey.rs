//! SSH 主机密钥校验（known_hosts）。
//!
//! SFTP 管理的是服务器上的真实文件，**中间人攻击的后果很严重**，
//! 因此本模块在建立 SSH 连接时强制校验服务端主机密钥：
//!
//! - **已知主机且公钥一致** → 接受；
//! - **已知主机但公钥不一致** → 拒绝，报「主机密钥已变更，可能是中间人攻击」；
//! - **未知主机** → 默认拒绝，提示运维先把主机公钥写入 known_hosts；
//! - 运维**显式**选择接受未知主机时，必须给出批准人，并写审计日志。
//!
//! ## 上游 API 的实际情况
//!
//! 本模块依赖的两个上游 API 能力**不同**，分工如下：
//!
//! - `ssh_key::known_hosts::KnownHosts`（`russh::keys::ssh_key::known_hosts::KnownHosts`）
//!   是**纯解析器**：`KnownHosts::new(&str)` 逐行解析，`read_file(path)` 读文件，
//!   产出 `Entry`（可取 `marker()` / `host_patterns()` / `public_key()`）。
//!   它**不提供**主机模式匹配，也不做公钥比对。
//! - `russh::keys::known_hosts::known_host_keys_path(host, port, path)` 是**文件级查找**，
//!   能正确处理**哈希主机名**（`|1|salt|hash`，需要 HMAC-SHA1），
//!   但它是**基于路径**的，且不区分 `@revoked` 标记。
//!
//! 因此本模块的策略是：
//!
//! - 明文主机名（`host`、`[host]:port`、通配符、`!` 否定）由本模块自行解析与匹配
//!   （用 `KnownHosts` 解析每一行，标记 `@revoked` 也能正确处理）；
//! - 哈希主机名委托给 `russh` 的文件级查找补齐；
//! - 最终的「接受 / 拒绝 / 未知」判定只有一个地方：[`decide_host_key`]。

use std::path::Path;

use atlas_core::{AtlasError, Result};
use russh::keys::ssh_key::known_hosts::{Entry, HostPatterns, KnownHosts, Marker};
use russh::keys::{HashAlg, PublicKey};

/// 标准 SSH 端口（known_hosts 中该端口不写端口号）。
const DEFAULT_SSH_PORT: u16 = 22;

/// 哈希主机名的前缀（`|1|salt|hash`）。
const HASHED_NAME_PREFIX: &str = "|1|";

/// 主机密钥校验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// known_hosts 中已记录该主机，且公钥一致——可以接受。
    Trusted,
    /// known_hosts 中已记录该主机，但同类型公钥不一致——疑似中间人攻击。
    Changed,
    /// known_hosts 中把该主机的公钥标记为 `@revoked`。
    Revoked,
    /// known_hosts 中没有该主机的记录。
    Unknown,
}

/// 未知主机的处理策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownHostPolicy {
    /// 默认策略：拒绝连接。
    Reject,
    /// 显式接受未知主机。
    ///
    /// 只能由调用方主动选择；接受时会写审计日志（含批准人）。
    Accept {
        /// 批准本次例外的人（用于审计日志）。
        approved_by: String,
    },
}

/// known_hosts 中与某个主机匹配的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostEntry {
    /// 在 known_hosts 文件中的行号（从 1 开始）。
    pub line: usize,
    /// 记录的主机公钥。
    pub public_key: PublicKey,
    /// 该记录是否被标记为 `@revoked`（已吊销）。
    pub revoked: bool,
}

/// 纯函数：根据「该主机已记录的公钥」与「服务端公钥」给出校验结论。
///
/// 判定规则（与 `russh` 自带的 `check_known_hosts_path` 一致）：
///
/// - 没有任何记录 → [`HostKeyDecision::Unknown`]；
/// - 有记录且公钥完全一致 → [`HostKeyDecision::Trusted`]；
/// - 有记录、同算法但公钥不同 → [`HostKeyDecision::Changed`]（疑似中间人）；
/// - 公钥被标记为吊销 → [`HostKeyDecision::Revoked`]；
/// - 有记录但算法都不同（服务端换了主机密钥类型）→ [`HostKeyDecision::Unknown`]。
pub fn decide_host_key(recorded: &[KnownHostEntry], server_key: &PublicKey) -> HostKeyDecision {
    if recorded.is_empty() {
        return HostKeyDecision::Unknown;
    }

    if recorded
        .iter()
        .any(|entry| entry.revoked && entry.public_key == *server_key)
    {
        return HostKeyDecision::Revoked;
    }

    if recorded
        .iter()
        .any(|entry| !entry.revoked && entry.public_key == *server_key)
    {
        return HostKeyDecision::Trusted;
    }

    if recorded
        .iter()
        .any(|entry| !entry.revoked && entry.public_key.algorithm() == server_key.algorithm())
    {
        return HostKeyDecision::Changed;
    }

    HostKeyDecision::Unknown
}

/// 纯函数：解析 known_hosts 文本并直接给出校验结论。
///
/// 输入是 known_hosts 文件的**内容**、主机名、端口与服务端公钥；
/// 输出是接受 / 拒绝 / 未知。完全不做 I/O，便于离线单测。
///
/// # 已知限制
///
/// 哈希主机名（`|1|salt|hash`）需要 HMAC-SHA1 才能验证，当前依赖集中
/// 没有可用的 HMAC-SHA1 实现，因此这类条目在本函数里**会被跳过**；
/// 生产路径走 [`load_known_host_keys`]，由 `russh` 负责哈希名匹配。
///
/// # 示例
///
/// ```
/// use atlas_sftp::hostkey::{evaluate_known_hosts, HostKeyDecision};
/// use russh::keys::parse_public_key_base64;
///
/// let key = parse_public_key_base64(
///     "AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti",
/// )
/// .unwrap();
/// let known_hosts = "example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti\n";
/// assert_eq!(
///     evaluate_known_hosts(known_hosts, "example.com", 22, &key),
///     HostKeyDecision::Trusted
/// );
/// ```
pub fn evaluate_known_hosts(
    known_hosts: &str,
    host: &str,
    port: u16,
    server_key: &PublicKey,
) -> HostKeyDecision {
    let recorded = collect_known_host_keys(known_hosts, host, port);
    decide_host_key(&recorded, server_key)
}

/// 纯函数：从 known_hosts 文本中找出与主机匹配的记录。
///
/// 返回的记录按文件顺序排列，行号从 1 开始。
///
/// 只处理**明文**主机名模式；哈希主机名（`|1|...`）在这里无法验证，
/// 见 [`load_known_host_keys`]。
pub fn collect_known_host_keys(known_hosts: &str, host: &str, port: u16) -> Vec<KnownHostEntry> {
    let token = host_token(host, port);
    let mut out = Vec::new();

    for (index, line) in known_hosts.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(entry) = parse_entry(trimmed) else {
            // 解析不了的行直接跳过：坏行不能让整个文件失效。
            continue;
        };

        // `@cert-authority` 记录的是 CA 的公钥，不是主机公钥，
        // 不能拿来和主机密钥比对（Atlas 不支持主机证书）。
        if entry.marker() == Some(&Marker::CertAuthority) {
            continue;
        }

        if !host_patterns_match(entry.host_patterns(), &token) {
            continue;
        }

        out.push(KnownHostEntry {
            line: index + 1,
            public_key: entry.public_key().clone(),
            revoked: entry.marker() == Some(&Marker::Revoked),
        });
    }

    out
}

/// 判断 known_hosts 文本中是否含有哈希主机名。
///
/// 哈希主机名只能通过文件路径方式校验（`russh` 的 API 基于路径），
/// 内存文本方式无法验证。
pub fn has_hashed_host_name(known_hosts: &str) -> bool {
    known_hosts.lines().any(|line| {
        // 主机名可能带 `@revoked` 之类的标记前缀，因此逐个字段检查。
        line.split_whitespace()
            .any(|field| field.starts_with(HASHED_NAME_PREFIX))
    })
}

/// 从 known_hosts 文件读取该主机已记录的公钥。
///
/// - 明文主机名由本模块解析与匹配（`@revoked` 标记也能正确处理）；
/// - 哈希主机名委托给 `russh::keys::known_hosts::known_host_keys_path`；
/// - 文件不存在时按「没有任何记录」处理（调用方据此报「未知主机」）。
///
/// # 错误
///
/// 读取文件失败（除「文件不存在」外）时返回 [`AtlasError::Sftp`]。
pub fn load_known_host_keys(host: &str, port: u16, path: &Path) -> Result<Vec<KnownHostEntry>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AtlasError::Sftp(format!(
                "读取 known_hosts 文件 {} 失败: {e}",
                path.display()
            )));
        }
    };

    let mut entries = collect_known_host_keys(&content, host, port);

    if has_hashed_host_name(&content) {
        // 哈希主机名需要 HMAC-SHA1，只有 russh 的文件级查找能做。
        // 它失败时（例如文件里混有 `@cert-authority` 标记行）不致命：
        // 哈希条目缺失只会让主机退化成「未知」，而未知默认被拒绝。
        if let Ok(hashed) = russh::keys::known_hosts::known_host_keys_path(host, port, path) {
            for (line, public_key) in hashed {
                if !entries.iter().any(|entry| entry.public_key == public_key) {
                    entries.push(KnownHostEntry {
                        line,
                        public_key,
                        revoked: false,
                    });
                }
            }
        }
    }

    Ok(entries)
}

/// 校验服务端主机密钥。
///
/// 这是连接时真正调用的入口：读取 known_hosts → 判定 → 按策略处理未知主机。
///
/// # 错误
///
/// - 主机密钥与记录不一致 → 「主机密钥已变更，可能是中间人攻击」；
/// - 主机密钥被吊销 → 「主机密钥已被吊销」；
/// - 未知主机且策略为 [`UnknownHostPolicy::Reject`] → 「未知主机」。
///
/// 错误信息只包含主机、端口、指纹与文件路径，**不含任何凭据内容**。
pub fn verify_server_key(
    host: &str,
    port: u16,
    server_key: &PublicKey,
    known_hosts_path: &Path,
    policy: &UnknownHostPolicy,
) -> Result<()> {
    let recorded = load_known_host_keys(host, port, known_hosts_path)?;

    match decide_host_key(&recorded, server_key) {
        HostKeyDecision::Trusted => {
            tracing::debug!(
                host = %host,
                port,
                fingerprint = %fingerprint_of(server_key),
                "SSH 主机密钥校验通过"
            );
            Ok(())
        }
        HostKeyDecision::Changed => Err(AtlasError::Sftp(format!(
            "主机密钥已变更，可能是中间人攻击：{host}:{port}（当前指纹 {}）",
            fingerprint_of(server_key)
        ))),
        HostKeyDecision::Revoked => Err(AtlasError::Sftp(format!(
            "主机密钥已被吊销：{host}:{port}（当前指纹 {}）",
            fingerprint_of(server_key)
        ))),
        HostKeyDecision::Unknown => match policy {
            UnknownHostPolicy::Reject => Err(AtlasError::Sftp(format!(
                "未知主机 {host}:{port}：请先把主机公钥写入 known_hosts 文件 {}",
                known_hosts_path.display()
            ))),
            UnknownHostPolicy::Accept { approved_by } => {
                tracing::warn!(
                    host = %host,
                    port,
                    fingerprint = %fingerprint_of(server_key),
                    approved_by = %approved_by,
                    known_hosts = %known_hosts_path.display(),
                    "已接受未知主机的 SSH 主机密钥：未做已知主机校验，存在中间人攻击风险"
                );
                Ok(())
            }
        },
    }
}

/// 计算主机公钥的 SHA-256 指纹（形如 `SHA256:...`）。
pub fn fingerprint_of(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 生成用于匹配的主机标识。
///
/// 标准端口写 `host`，非标准端口写 `[host]:port`——与 OpenSSH 的写法一致。
fn host_token(host: &str, port: u16) -> String {
    if port == DEFAULT_SSH_PORT {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// 解析 known_hosts 的一行。
fn parse_entry(line: &str) -> Option<Entry> {
    let mut parser = KnownHosts::new(line);
    match parser.next() {
        Some(Ok(entry)) => Some(entry),
        Some(Err(_)) | None => None,
    }
}

/// 判断条目的主机模式是否匹配目标主机。
///
/// - `!` 开头的否定模式优先：一旦匹配，该条目整体作废；
/// - 其余模式按通配符匹配；
/// - 哈希主机名无法在内存中验证，这里一律不匹配。
fn host_patterns_match(patterns: &HostPatterns, token: &str) -> bool {
    match patterns {
        HostPatterns::Patterns(list) => {
            let mut matched = false;
            for pattern in list {
                if let Some(negated) = pattern.strip_prefix('!') {
                    if glob_matches(negated, token) {
                        return false;
                    }
                } else if glob_matches(pattern, token) {
                    matched = true;
                }
            }
            matched
        }
        HostPatterns::HashedName { .. } => false,
    }
}

/// 通配符匹配：`*` 匹配任意长度，`?` 匹配单个字符。
fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // 最近一次遇到的 `*` 的位置，以及它当时匹配到的文本位置。
    let (mut star, mut mark) = (usize::MAX, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            // 回溯：让上一个 `*` 多吞一个字符。
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的 ed25519 公钥（来自 `russh` 自身的测试数据）。
    const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    /// 测试用的另一把 ed25519 公钥。
    const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
    /// 哈希主机名条目对应的公钥（`russh` 测试数据里配对的公钥）。
    const KEY_HASHED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";

    fn key(base64: &str) -> PublicKey {
        russh::keys::parse_public_key_base64(base64).expect("测试公钥应当可解析")
    }

    #[test]
    fn known_host_with_matching_key_is_trusted() {
        let content = format!("example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A)),
            HostKeyDecision::Trusted
        );
    }

    #[test]
    fn known_host_with_different_key_is_changed() {
        let content = format!("example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 22, &key(KEY_B)),
            HostKeyDecision::Changed
        );
    }

    #[test]
    fn unknown_host_is_unknown() {
        let content = format!("example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "other.com", 22, &key(KEY_A)),
            HostKeyDecision::Unknown
        );
        assert_eq!(
            evaluate_known_hosts("", "example.com", 22, &key(KEY_A)),
            HostKeyDecision::Unknown
        );
    }

    #[test]
    fn non_standard_port_uses_bracket_form() {
        let content = format!("[example.com]:2222 ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 2222, &key(KEY_A)),
            HostKeyDecision::Trusted
        );
        // 端口不匹配时不应命中。
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A)),
            HostKeyDecision::Unknown
        );
    }

    #[test]
    fn wildcard_and_negation_patterns_are_honoured() {
        let wildcard = format!("*.example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&wildcard, "mc.example.com", 22, &key(KEY_A)),
            HostKeyDecision::Trusted
        );

        let negated = format!("!mc.example.com,*.example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&negated, "mc.example.com", 22, &key(KEY_A)),
            HostKeyDecision::Unknown
        );
        assert_eq!(
            evaluate_known_hosts(&negated, "other.example.com", 22, &key(KEY_A)),
            HostKeyDecision::Trusted
        );
    }

    #[test]
    fn revoked_entry_is_rejected() {
        let content = format!("@revoked example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A)),
            HostKeyDecision::Revoked
        );
    }

    #[test]
    fn cert_authority_entry_is_not_a_host_key() {
        // `@cert-authority` 记录的是 CA 公钥，不能当成主机公钥比对，
        // 否则会误报「主机密钥已变更」。
        let content = format!("@cert-authority *.example.com ssh-ed25519 {KEY_A}\n");
        assert_eq!(
            evaluate_known_hosts(&content, "mc.example.com", 22, &key(KEY_A)),
            HostKeyDecision::Unknown
        );
    }

    #[test]
    fn hashed_name_behind_marker_is_detected() {
        let content = format!(
            "@revoked |1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= ssh-ed25519 {KEY_HASHED}\n"
        );
        assert!(has_hashed_host_name(&content));
    }

    #[test]
    fn comments_and_broken_lines_are_skipped() {
        let content = format!(
            "# 这是注释\ngarbage line without key\nexample.com ssh-ed25519 {KEY_A} # 尾注\n"
        );
        assert_eq!(
            evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A)),
            HostKeyDecision::Trusted
        );
    }

    #[test]
    fn decide_host_key_handles_empty_records() {
        assert_eq!(decide_host_key(&[], &key(KEY_A)), HostKeyDecision::Unknown);
    }

    #[test]
    fn hashed_host_name_is_detected() {
        let content = format!(
            "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= ssh-ed25519 {KEY_HASHED}\n"
        );
        assert!(has_hashed_host_name(&content));
        assert!(!has_hashed_host_name(&format!(
            "example.com ssh-ed25519 {KEY_A}\n"
        )));
    }

    #[test]
    fn hashed_host_name_is_resolved_from_file() {
        // russh 的测试数据：哈希主机名 `|1|...` 对应 example.com:22。
        let content = format!(
            "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= ssh-ed25519 {KEY_HASHED}\n"
        );
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, &content).expect("写入 known_hosts 失败");

        let recorded =
            load_known_host_keys("example.com", 22, &path).expect("读取 known_hosts 失败");
        assert_eq!(
            decide_host_key(&recorded, &key(KEY_HASHED)),
            HostKeyDecision::Trusted
        );
    }

    #[test]
    fn missing_known_hosts_file_yields_no_records() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("not-exist");
        let recorded = load_known_host_keys("example.com", 22, &path).expect("文件缺失不应报错");
        assert!(recorded.is_empty());
    }

    #[test]
    fn verify_server_key_rejects_unknown_host_by_default() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, format!("example.com ssh-ed25519 {KEY_A}\n")).expect("写入失败");

        let err = verify_server_key(
            "other.com",
            22,
            &key(KEY_A),
            &path,
            &UnknownHostPolicy::Reject,
        )
        .expect_err("未知主机必须被拒绝");
        let msg = err.to_string();
        assert!(msg.contains("未知主机"), "错误信息应说明是未知主机: {msg}");
    }

    #[test]
    fn verify_server_key_reports_changed_key() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, format!("example.com ssh-ed25519 {KEY_A}\n")).expect("写入失败");

        let err = verify_server_key(
            "example.com",
            22,
            &key(KEY_B),
            &path,
            &UnknownHostPolicy::Reject,
        )
        .expect_err("密钥变更必须被拒绝");
        let msg = err.to_string();
        assert!(
            msg.contains("主机密钥已变更，可能是中间人攻击"),
            "错误信息应指出疑似中间人攻击: {msg}"
        );
    }

    #[test]
    fn verify_server_key_accepts_unknown_host_when_approved() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, "").expect("写入失败");

        let policy = UnknownHostPolicy::Accept {
            approved_by: "运维张三".to_string(),
        };
        verify_server_key("example.com", 22, &key(KEY_A), &path, &policy)
            .expect("显式批准后应放行");
    }

    #[test]
    fn verify_server_key_errors_contain_no_credentials() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, format!("example.com ssh-ed25519 {KEY_A}\n")).expect("写入失败");

        let err = verify_server_key(
            "example.com",
            22,
            &key(KEY_B),
            &path,
            &UnknownHostPolicy::Reject,
        )
        .expect_err("密钥变更必须被拒绝");
        let msg = err.to_string();
        assert!(!msg.contains("PRIVATE KEY"));
        assert!(!msg.contains("password"));
        // 指纹可以出现在错误信息里，方便运维比对。
        assert!(msg.contains("SHA256:"));
    }

    #[test]
    fn glob_matching_handles_wildcards() {
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*.example.com", "mc.example.com"));
        assert!(!glob_matches("*.example.com", "example.org"));
        assert!(glob_matches("mc?.example.com", "mc1.example.com"));
        assert!(!glob_matches("mc?.example.com", "mc12.example.com"));
        assert!(glob_matches("exact", "exact"));
        assert!(!glob_matches("exact", "exactly"));
    }
}
