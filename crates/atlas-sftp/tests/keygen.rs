//! SSH 客户端密钥对生成的测试。
//!
//! 关键点：
//! - 生成的私钥必须能被 `russh` **解析回来**（否则 SFTP 根本用不了）；
//! - 从私钥推导出的公钥必须与写出的 `.pub` 文件一致；
//! - 已存在的私钥不能被静默覆盖（覆盖 = 销毁旧凭据）。

use atlas_sftp::KeyFormat;
use atlas_sftp::keygen::{generate_ed25519, generate_ed25519_with, public_path_of};
use russh::keys::decode_secret_key;

const PASSPHRASE: &str = "correct-horse-battery-staple";

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
    // 公钥文件名与私钥格式无关：`with_extension` 是**替换**扩展名，
    // 所以 `id_ed25519.pem` 的公钥仍是 `id_ed25519.pub`。
    assert_eq!(
        public_path_of(std::path::Path::new("keys/id_ed25519.pem")),
        std::path::PathBuf::from("keys/id_ed25519.pub")
    );
}

// ------------------------------------------------------------------ 格式

#[test]
fn pkcs8_格式产出标准_pem_私钥() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("id_ed25519.pem");

    let pair = generate_ed25519_with(&private_path, None, false, KeyFormat::Pkcs8, None)
        .expect("生成应成功");

    assert_eq!(pair.format, KeyFormat::Pkcs8);
    assert!(!pair.encrypted);

    let pem = std::fs::read_to_string(&pair.private_path).expect("读取私钥失败");
    assert!(
        pem.starts_with("-----BEGIN PRIVATE KEY-----"),
        "PKCS#8 应是标准 PEM 头，实际：{}",
        pem.lines().next().unwrap_or_default()
    );

    // 仍须能被解析回来。
    let parsed = decode_secret_key(&pem, None).expect("PKCS#8 私钥应能被解析");
    assert_eq!(
        parsed.public_key().to_openssh().expect("导出公钥失败"),
        pair.public_key
    );
}

#[test]
fn pkcs8_加口令后能被口令解开且公钥一致() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("id_ed25519.pem");

    let pair = generate_ed25519_with(
        &private_path,
        Some("atlas-test"),
        false,
        KeyFormat::Pkcs8,
        Some(PASSPHRASE),
    )
    .expect("生成应成功");

    assert!(pair.encrypted, "应标记为已加密");

    let pem = std::fs::read_to_string(&pair.private_path).expect("读取私钥失败");
    assert!(
        pem.starts_with("-----BEGIN ENCRYPTED PRIVATE KEY-----"),
        "加密私钥应有 ENCRYPTED 头"
    );
    assert!(!pem.contains(PASSPHRASE), "私钥文件里不得出现明文口令");

    // 用正确口令能解开，且公钥一致。
    let parsed = decode_secret_key(&pem, Some(PASSPHRASE)).expect("正确口令应能解开私钥");
    assert_eq!(
        parsed.public_key().to_openssh().expect("导出公钥失败"),
        pair.public_key.trim_end_matches(" atlas-test")
    );

    // 用错误口令必须失败。
    assert!(
        decode_secret_key(&pem, Some("wrong-passphrase")).is_err(),
        "错误口令必须解析失败"
    );
}

#[test]
fn openssh_格式不支持口令加密() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let err = generate_ed25519_with(
        &dir.path().join("id_ed25519"),
        None,
        false,
        KeyFormat::OpenSsh,
        Some(PASSPHRASE),
    )
    .expect_err("OpenSSH 格式应拒绝口令");
    assert!(
        err.to_string().contains("pkcs8"),
        "错误信息应指向 pkcs8，实际：{err}"
    );
    assert!(
        !dir.path().join("id_ed25519").exists(),
        "失败的生成不得留下私钥文件"
    );
}

#[test]
fn 格式名可解析且默认文件名不同() {
    assert_eq!(KeyFormat::parse("openssh"), Some(KeyFormat::OpenSsh));
    assert_eq!(KeyFormat::parse("OpenSSH"), Some(KeyFormat::OpenSsh));
    assert_eq!(KeyFormat::parse("pkcs8"), Some(KeyFormat::Pkcs8));
    assert_eq!(KeyFormat::parse("pem"), Some(KeyFormat::Pkcs8));
    assert_eq!(KeyFormat::parse("不存在的格式"), None);

    assert_eq!(KeyFormat::OpenSsh.default_file_name(), "id_ed25519");
    assert_eq!(KeyFormat::Pkcs8.default_file_name(), "id_ed25519.pem");
}

/// Windows 上私钥的 ACL 必须被收紧。
///
/// 否则会有两个问题：
/// 1. 安全——私钥继承工作区目录的 ACL，同机其它账户可能可读；
/// 2. 可用性——Windows 自带的 `ssh`/`scp` 见到过宽的 ACL 会直接拒绝使用该私钥
///    （"UNPROTECTED PRIVATE KEY FILE"）。
#[cfg(windows)]
#[test]
fn windows_下私钥_acl_被收紧到仅管理员与系统() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let private_path = dir.path().join("keys").join("id_ed25519");

    generate_ed25519(&private_path, None, false).expect("生成应成功");

    let output = std::process::Command::new("icacls")
        .arg(&private_path)
        .output()
        .expect("应能调用 icacls");
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(text.contains("SYSTEM"), "ACL 应保留 SYSTEM：\n{text}");
    assert!(
        text.contains("Administrators") || text.contains("-1-5-32-544"),
        "ACL 应保留 Administrators：\n{text}"
    );
    assert!(
        !text.contains("(I)"),
        "不应残留继承自目录的权限项：\n{text}"
    );
    // 注意只能匹配**组名**，不能匹配整段输出——文件路径里本身就有 "Users"
    // （C:\Users\...），整段匹配会误判。
    assert!(
        !text.contains("Everyone")
            && !text.contains("BUILTIN\\Users")
            && !text.contains("-1-5-32-545"),
        "不应把私钥开放给普通用户：\n{text}"
    );
}
