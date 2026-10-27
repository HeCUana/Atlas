//! SSH 客户端密钥对生成的测试。
//!
//! 关键点：
//! - 生成的私钥必须能被 `russh` **解析回来**（否则 SFTP 根本用不了）；
//! - 从私钥推导出的公钥必须与写出的 `.pub` 文件一致；
//! - 已存在的私钥不能被静默覆盖（覆盖 = 销毁旧凭据）。

use atlas_sftp::keygen::{generate_ed25519, public_path_of};
use russh::keys::decode_secret_key;

#[test]
fn 生成的密钥对可用且公私钥匹配() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("keys").join("id_ed25519");

    let pair = generate_ed25519(&private_path, Some("atlas-test"), false).expect("生成应成功");

    assert!(pair.private_path.exists(), "私钥文件应存在");
    assert!(pair.public_path.exists(), "公钥文件应存在");
    assert_eq!(pair.public_path, public_path_of(&pair.private_path));
    assert!(
        pair.public_key.starts_with("ssh-ed25519 "),
        "公钥格式应正确：{}",
        pair.public_key
    );
    assert!(
        pair.public_key.ends_with(" atlas-test"),
        "公钥应带注释：{}",
        pair.public_key
    );
    assert!(
        pair.fingerprint.starts_with("SHA256:"),
        "指纹格式应正确：{}",
        pair.fingerprint
    );

    // 私钥必须能被 russh 解析回来——这是 SFTP 能用它的前提。
    let pem = std::fs::read_to_string(&pair.private_path).expect("读取私钥失败");
    assert!(
        pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"),
        "应是 OpenSSH 私钥格式"
    );
    let parsed = decode_secret_key(&pem, None).expect("私钥应能被解析");

    // 从私钥推导出的公钥必须与 .pub 文件内容一致。
    let derived = parsed.public_key().to_openssh().expect("应能导出公钥");
    assert_eq!(
        derived,
        pair.public_key.trim_end_matches(" atlas-test"),
        "私钥推导出的公钥应与写出的公钥一致"
    );

    // .pub 文件内容 = 公钥 + 换行
    let pub_file = std::fs::read_to_string(&pair.public_path).expect("读取公钥失败");
    assert_eq!(pub_file, format!("{}\n", pair.public_key));
}

#[test]
fn 私钥目录会被自动创建() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("nested").join("deeper").join("id_ed25519");

    generate_ed25519(&private_path, None, false).expect("生成应成功");

    assert!(
        private_path.parent().expect("应有父目录").is_dir(),
        "私钥目录应被自动创建"
    );
}

#[test]
fn 已存在的私钥不会被静默覆盖() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("id_ed25519");

    let first = generate_ed25519(&private_path, None, false).expect("首次生成应成功");
    let original = std::fs::read_to_string(&private_path).expect("读取私钥失败");

    let err = generate_ed25519(&private_path, None, false).expect_err("二次生成应报错");
    assert!(
        err.to_string().contains("已存在"),
        "错误信息应说明私钥已存在，实际：{err}"
    );

    // 原有私钥必须完好无损。
    assert_eq!(
        std::fs::read_to_string(&private_path).expect("读取私钥失败"),
        original,
        "失败的生成不得改动已有私钥"
    );
    assert!(first.fingerprint.starts_with("SHA256:"));
}

#[test]
fn force_会覆盖已存在的私钥() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("id_ed25519");

    let first = generate_ed25519(&private_path, None, false).expect("首次生成应成功");
    let second = generate_ed25519(&private_path, None, true).expect("force 覆盖应成功");

    assert_ne!(
        first.fingerprint, second.fingerprint,
        "两次生成的密钥应不同"
    );
}

#[test]
fn 两次生成的密钥互不相同() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let a = generate_ed25519(&dir.path().join("a"), None, false).expect("生成应成功");
    let b = generate_ed25519(&dir.path().join("b"), None, false).expect("生成应成功");
    assert_ne!(a.fingerprint, b.fingerprint, "不同密钥的指纹应不同");
}

#[test]
fn 公钥路径规则是同目录换扩展名() {
    assert_eq!(
        public_path_of(std::path::Path::new("keys/id_ed25519")),
        std::path::PathBuf::from("keys/id_ed25519.pub")
    );
}
