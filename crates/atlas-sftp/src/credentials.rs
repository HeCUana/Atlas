//! SFTP 凭据。
//!
//! 凭据里同时可能出现口令、私钥正文与私钥口令，三者都属于机密。
//! 本模块的硬性约定：
//!
//! - [`SftpCredentials`] 的 `Debug` 是**手写实现**，敏感字段一律脱敏；
//! - 任何错误信息只描述「哪一步失败」，绝不回显机密内容；
//! - 私钥只在需要认证时从文件读入内存（[`SftpCredentials::from_config`]），
//!   避免长期持有文件句柄。

use std::fmt;
use std::path::Path;

use atlas_core::{AtlasError, Result, SftpConfig};

/// 脱敏占位符。
const REDACTED: &str = "***";

/// SFTP 认证凭据。
///
/// 口令与私钥二者至少要提供其一。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SftpCredentials {
    /// 用户名。
    pub username: String,
    /// 口令。
    pub password: Option<String>,
    /// 私钥正文（PEM / OpenSSH 格式文本，已从文件读入内存）。
    pub private_key: Option<String>,
    /// 私钥口令。
    pub passphrase: Option<String>,
}

impl fmt::Debug for SftpCredentials {
    /// 手写 `Debug`：口令、私钥正文、私钥口令一律替换为 `***`。
    ///
    /// 只保留「是否设置了该字段」这一信息，不泄露任何机密内容。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SftpCredentials")
            .field("username", &self.username)
            .field("password", &redacted(self.password.is_some()))
            .field("private_key", &redacted(self.private_key.is_some()))
            .field("passphrase", &redacted(self.passphrase.is_some()))
            .finish()
    }
}

/// 把「是否存在」转换成脱敏后的占位符。
fn redacted(present: bool) -> Option<&'static str> {
    if present { Some(REDACTED) } else { None }
}

impl SftpCredentials {
    /// 仅用口令构造。
    pub fn password(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: Some(password.into()),
            private_key: None,
            passphrase: None,
        }
    }

    /// 用私钥正文构造。
    ///
    /// `private_key` 为 PEM / OpenSSH 格式文本；解析交给 `russh` 完成。
    pub fn key(username: impl Into<String>, private_key: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: None,
            private_key: Some(private_key.into()),
            passphrase: None,
        }
    }

    /// 附上私钥口令。
    pub fn with_passphrase(mut self, passphrase: impl Into<String>) -> Self {
        self.passphrase = Some(passphrase.into());
        self
    }

    /// 从连接配置构造凭据。
    ///
    /// 私钥按 [`SftpConfig::key_path`] 指向的文件读入内存；
    /// 读文件失败时返回 [`AtlasError::Sftp`]（错误信息只含路径，不含私钥内容）。
    ///
    /// 私钥口令取自 [`SftpConfig::key_passphrase`]——用 `atlas keygen
    /// --format pkcs8 --passphrase ...` 生成的加密私钥需要它。
    pub fn from_config(config: &SftpConfig) -> Result<Self> {
        let private_key = match &config.key_path {
            Some(path) => Some(read_private_key(path)?),
            None => None,
        };

        Ok(Self {
            username: config.username.clone(),
            password: config.password.clone(),
            private_key,
            passphrase: config.key_passphrase.clone(),
        })
    }

    /// 凭据是否可用（至少有口令或私钥）。
    pub fn is_usable(&self) -> bool {
        self.password.is_some() || self.private_key.is_some()
    }

    /// 实际使用的认证方式，用于日志（不泄露机密）。
    pub fn auth_method(&self) -> &'static str {
        if self.private_key.is_some() {
            "私钥"
        } else if self.password.is_some() {
            "口令"
        } else {
            "无"
        }
    }
}

/// 读取私钥文件正文。
fn read_private_key(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| AtlasError::Sftp(format!("读取私钥文件 {} 失败: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_PASSWORD: &str = "s3cr3t-p@ssw0rd";
    const SECRET_KEY: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nZmFrZWtleQ==\n-----END OPENSSH PRIVATE KEY-----";
    const SECRET_PASSPHRASE: &str = "s3cr3t-passphrase";

    #[test]
    fn debug_hides_password() {
        let creds = SftpCredentials::password("mc", SECRET_PASSWORD);
        let rendered = format!("{creds:?}");
        assert!(!rendered.contains(SECRET_PASSWORD), "口令泄漏: {rendered}");
        assert!(rendered.contains(REDACTED));
        assert!(rendered.contains("mc"));
    }

    #[test]
    fn debug_hides_private_key_and_passphrase() {
        let creds = SftpCredentials::key("mc", SECRET_KEY).with_passphrase(SECRET_PASSPHRASE);
        let rendered = format!("{creds:?}");
        assert!(!rendered.contains(SECRET_KEY), "私钥泄漏: {rendered}");
        assert!(
            !rendered.contains(SECRET_PASSPHRASE),
            "私钥口令泄漏: {rendered}"
        );
        assert!(
            !rendered.contains("BEGIN OPENSSH PRIVATE KEY"),
            "私钥泄漏: {rendered}"
        );
    }

    #[test]
    fn debug_of_empty_credentials_has_no_secrets() {
        let creds = SftpCredentials::default();
        let rendered = format!("{creds:?}");
        assert!(rendered.contains("SftpCredentials"));
        assert!(rendered.contains("username"));
        assert!(rendered.contains("password"));
    }

    #[test]
    fn is_usable_reflects_available_material() {
        assert!(!SftpCredentials::default().is_usable());
        assert!(SftpCredentials::password("mc", SECRET_PASSWORD).is_usable());
        assert!(SftpCredentials::key("mc", SECRET_KEY).is_usable());
    }

    #[test]
    fn auth_method_reports_without_leaking() {
        assert_eq!(SftpCredentials::default().auth_method(), "无");
        assert_eq!(
            SftpCredentials::password("mc", SECRET_PASSWORD).auth_method(),
            "口令"
        );
        assert_eq!(SftpCredentials::key("mc", SECRET_KEY).auth_method(), "私钥");
    }

    #[test]
    fn from_config_reads_private_key_into_memory() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let key_path = dir.path().join("id_ed25519");
        std::fs::write(&key_path, SECRET_KEY).expect("写入临时私钥失败");

        let config = SftpConfig {
            host: "127.0.0.1".to_string(),
            port: 22,
            username: "mc".to_string(),
            password: Some(SECRET_PASSWORD.to_string()),
            key_path: Some(key_path.clone()),
            key_passphrase: None,
            root: None,
        };

        let creds = SftpCredentials::from_config(&config).expect("构造凭据失败");
        assert_eq!(creds.username, "mc");
        assert_eq!(creds.password.as_deref(), Some(SECRET_PASSWORD));
        assert_eq!(creds.private_key.as_deref(), Some(SECRET_KEY));
        assert!(creds.is_usable());

        // 脱敏后不得出现任何机密。
        let rendered = format!("{creds:?}");
        assert!(!rendered.contains(SECRET_PASSWORD));
        assert!(!rendered.contains(SECRET_KEY));
    }

    #[test]
    fn from_config_reports_missing_key_file() {
        let config = SftpConfig {
            host: "127.0.0.1".to_string(),
            port: 22,
            username: "mc".to_string(),
            password: None,
            key_path: Some(std::path::PathBuf::from("definitely-missing-key")),
            key_passphrase: None,
            root: None,
        };

        let err = SftpCredentials::from_config(&config).expect_err("应当因缺少私钥文件而失败");
        assert!(matches!(err, AtlasError::Sftp(_)));
    }
}
