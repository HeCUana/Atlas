//! 配置模板与"必要目录"推导的测试。

use std::path::PathBuf;

use atlas_core::config::DataSource;
use atlas_core::{AtlasConfig, ServerConfig};

fn base_server(id: &str, workdir: &str) -> ServerConfig {
    ServerConfig {
        id: id.into(),
        name: String::new(),
        workdir: PathBuf::from(workdir),
        command: "java".into(),
        args: Vec::new(),
        env: Default::default(),
        jvm_args: Vec::new(),
        auto_start: false,
        log_path: None,
    }
}

#[test]
fn 生成的模板是合法_toml_且能被读回() {
    let template = AtlasConfig::template_toml();
    let parsed: AtlasConfig =
        toml::from_str(&template).expect("模板必须是合法 TOML 且能解析为 AtlasConfig");
    parsed.validate().expect("模板必须通过校验");
    assert!(parsed.servers.is_empty(), "模板默认不带服务器");
    assert_eq!(parsed.plugin_dir, PathBuf::from("plugins"));
    assert_eq!(parsed.event_capacity, 1024);
}

#[test]
fn 模板包含关键配置项的说明() {
    let template = AtlasConfig::template_toml();
    for key in [
        "[[servers]]",
        "[data_source]",
        "plugin_dir",
        "event_capacity",
        "[sftp]",
        "log_profiles",
    ] {
        assert!(template.contains(key), "模板应包含 {key}");
    }
}

#[test]
fn 默认配置需要插件目录与日志目录() {
    let config = AtlasConfig::default();
    let dirs = config.required_dirs();
    assert!(dirs.contains(&PathBuf::from("plugins")));
    assert!(dirs.contains(&PathBuf::from("logs")));
}

#[test]
fn 服务器的工作目录也在必需目录里() {
    let config = AtlasConfig {
        servers: vec![base_server("survival", "servers/survival")],
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(dirs.contains(&PathBuf::from("servers/survival")));
}

#[test]
fn 日志路径像文件时取其父目录() {
    let config = AtlasConfig {
        data_source: DataSource::Log {
            path: PathBuf::from("logs/latest.log"),
        },
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(
        dirs.contains(&PathBuf::from("logs")),
        "带扩展名的路径应取父目录，实际为 {dirs:?}"
    );
    assert!(!dirs.contains(&PathBuf::from("logs/latest.log")));
}

#[test]
fn 日志路径是目录时取其自身() {
    let config = AtlasConfig {
        data_source: DataSource::Log {
            path: PathBuf::from("var/logs"),
        },
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(dirs.contains(&PathBuf::from("var/logs")));
}

#[test]
fn mod_数据源不需要日志目录() {
    let config = AtlasConfig {
        data_source: DataSource::Mod {
            host: "127.0.0.1".into(),
            port: 25565,
            token: None,
        },
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(!dirs.contains(&PathBuf::from("logs")));
    assert!(dirs.contains(&PathBuf::from("plugins")));
}

#[test]
fn 空的插件目录不会被算作必需目录() {
    let config = AtlasConfig {
        plugin_dir: PathBuf::new(),
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(!dirs.contains(&PathBuf::new()));
}

#[test]
fn sftp_私钥所在目录属于必需目录() {
    // 私钥文件不存在时连接必然失败，目录得先在。
    let config = AtlasConfig {
        sftp: Some(atlas_core::SftpConfig {
            host: "10.0.0.5".into(),
            port: 22,
            username: "mc".into(),
            password: None,
            key_path: Some(PathBuf::from("keys/id_ed25519")),
            root: None,
        }),
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(
        dirs.contains(&PathBuf::from("keys")),
        "私钥目录应被包含，实际 {dirs:?}"
    );
}

#[test]
fn sftp_私钥在绝对路径时取其父目录() {
    let config = AtlasConfig {
        sftp: Some(atlas_core::SftpConfig {
            host: "10.0.0.5".into(),
            port: 22,
            username: "mc".into(),
            password: None,
            key_path: Some(PathBuf::from("/etc/atlas/keys/id_ed25519")),
            root: None,
        }),
        ..Default::default()
    };
    let dirs = config.required_dirs();
    assert!(dirs.contains(&PathBuf::from("/etc/atlas/keys")));
}

#[test]
fn 没有_sftp_配置时不引入密钥目录() {
    let config = AtlasConfig::default();
    let dirs = config.required_dirs();
    assert!(!dirs.contains(&PathBuf::from("keys")));
}
