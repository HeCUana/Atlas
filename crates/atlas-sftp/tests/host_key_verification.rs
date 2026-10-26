//! SSH 主机密钥校验（known_hosts）的集成测试。
//!
//! 覆盖「已知主机且公钥一致 → 接受」「公钥不一致 → 拒绝」「未知主机 → 默认拒绝」
//! 以及显式接受未知主机的例外通道。全部离线执行，不需要真实 SSH 服务器。

use std::path::PathBuf;

use atlas_sftp::SftpConnectOptions;
use atlas_sftp::hostkey::{
    HostKeyDecision, KnownHostEntry, UnknownHostPolicy, decide_host_key, evaluate_known_hosts,
    has_hashed_host_name, load_known_host_keys, verify_server_key,
};
use russh::keys::{PublicKey, parse_public_key_base64};

/// 测试用 ed25519 公钥 A（来自 `russh` 自身的测试数据）。
const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
/// 测试用 ed25519 公钥 B。
const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
/// 哈希主机名条目对应的公钥。
const KEY_HASHED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";

fn key(base64: &str) -> PublicKey {
    parse_public_key_base64(base64).expect("测试公钥应当可解析")
}

/// 写一个临时 known_hosts 文件，返回其路径。
fn write_known_hosts(content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let path = dir.path().join("known_hosts");
    std::fs::write(&path, content).expect("写入 known_hosts 失败");
    (dir, path)
}

#[test]
fn known_host_with_matching_key_is_accepted() {
    let (_dir, path) = write_known_hosts(&format!("example.com ssh-ed25519 {KEY_A}\n"));

    verify_server_key(
        "example.com",
        22,
        &key(KEY_A),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect("公钥一致时必须放行");
}

#[test]
fn known_host_with_mismatching_key_is_rejected() {
    let (_dir, path) = write_known_hosts(&format!("example.com ssh-ed25519 {KEY_A}\n"));

    let err = verify_server_key(
        "example.com",
        22,
        &key(KEY_B),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect_err("公钥不一致时必须拒绝");

    let msg = err.to_string();
    assert!(
        msg.contains("主机密钥已变更，可能是中间人攻击"),
        "错误信息应指出疑似中间人攻击: {msg}"
    );
}

#[test]
fn unknown_host_is_rejected_by_default() {
    let (_dir, path) = write_known_hosts(&format!("example.com ssh-ed25519 {KEY_A}\n"));

    let err = verify_server_key(
        "unknown.example.org",
        22,
        &key(KEY_A),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect_err("未知主机必须默认拒绝");

    let msg = err.to_string();
    assert!(msg.contains("未知主机"), "错误信息应说明是未知主机: {msg}");
    assert!(
        msg.contains("known_hosts"),
        "错误信息应提示写入 known_hosts: {msg}"
    );
}

#[test]
fn missing_known_hosts_file_rejects_unknown_host() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let path = dir.path().join("not-exist");

    let err = verify_server_key(
        "example.com",
        22,
        &key(KEY_A),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect_err("known_hosts 缺失时必须拒绝");

    assert!(err.to_string().contains("未知主机"));
}

#[test]
fn unknown_host_can_be_accepted_explicitly() {
    let (_dir, path) = write_known_hosts(&format!("example.com ssh-ed25519 {KEY_A}\n"));

    let policy = UnknownHostPolicy::Accept {
        approved_by: "运维王五".to_string(),
    };
    verify_server_key("unknown.example.org", 22, &key(KEY_A), &path, &policy)
        .expect("显式批准后应放行");
}

#[test]
fn revoked_host_key_is_rejected_even_when_approved() {
    let (_dir, path) = write_known_hosts(&format!("@revoked example.com ssh-ed25519 {KEY_A}\n"));

    let policy = UnknownHostPolicy::Accept {
        approved_by: "运维王五".to_string(),
    };
    let err = verify_server_key("example.com", 22, &key(KEY_A), &path, &policy)
        .expect_err("被吊销的密钥不得放行");
    assert!(err.to_string().contains("吊销"));
}

#[test]
fn non_standard_port_requires_bracket_form() {
    let (_dir, path) = write_known_hosts(&format!("[example.com]:2222 ssh-ed25519 {KEY_A}\n"));

    verify_server_key(
        "example.com",
        2222,
        &key(KEY_A),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect("端口匹配时必须放行");

    let err = verify_server_key(
        "example.com",
        22,
        &key(KEY_A),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect_err("端口不匹配时不得放行");
    assert!(err.to_string().contains("未知主机"));
}

#[test]
fn hashed_host_name_is_supported_via_file() {
    // `russh` 的测试数据：哈希主机名对应 example.com:22。
    let content = format!(
        "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= ssh-ed25519 {KEY_HASHED}\n"
    );
    let (_dir, path) = write_known_hosts(&content);

    assert!(has_hashed_host_name(&content));

    verify_server_key(
        "example.com",
        22,
        &key(KEY_HASHED),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect("哈希主机名应当能被文件级查找解析");
}

#[test]
fn pure_decision_is_independent_of_io() {
    // 纯函数：known_hosts 内容 + 主机 + 端口 + 服务端公钥 → 决策。
    let content = format!("example.com ssh-ed25519 {KEY_A}\n");
    assert_eq!(
        evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A)),
        HostKeyDecision::Trusted
    );
    assert_eq!(
        evaluate_known_hosts(&content, "example.com", 22, &key(KEY_B)),
        HostKeyDecision::Changed
    );
    assert_eq!(
        evaluate_known_hosts(&content, "other.com", 22, &key(KEY_A)),
        HostKeyDecision::Unknown
    );
}

#[test]
fn file_lookup_and_content_lookup_agree() {
    let content = format!("example.com ssh-ed25519 {KEY_A}\n");
    let (_dir, path) = write_known_hosts(&content);

    let recorded = load_known_host_keys("example.com", 22, &path).expect("读取 known_hosts 失败");
    assert_eq!(
        decide_host_key(&recorded, &key(KEY_A)),
        HostKeyDecision::Trusted
    );
    assert_eq!(
        decide_host_key(&recorded, &key(KEY_B)),
        HostKeyDecision::Changed
    );

    // 两条路径对同一份文件应当给出一致结论。
    let by_content = evaluate_known_hosts(&content, "example.com", 22, &key(KEY_A));
    assert_eq!(by_content, HostKeyDecision::Trusted);
}

#[test]
fn decide_host_key_on_empty_records_is_unknown() {
    let empty: Vec<KnownHostEntry> = Vec::new();
    assert_eq!(
        decide_host_key(&empty, &key(KEY_A)),
        HostKeyDecision::Unknown
    );
}

#[test]
fn error_messages_never_contain_secrets() {
    let (_dir, path) = write_known_hosts(&format!("example.com ssh-ed25519 {KEY_A}\n"));

    let err = verify_server_key(
        "example.com",
        22,
        &key(KEY_B),
        &path,
        &UnknownHostPolicy::Reject,
    )
    .expect_err("公钥不一致时必须拒绝");

    let msg = err.to_string();
    for secret in ["PRIVATE KEY", "password", "passphrase", "s3cr3t"] {
        assert!(!msg.contains(secret), "错误信息不应包含 {secret}: {msg}");
    }
}

#[test]
fn connect_options_default_to_strict_policy() {
    let options = SftpConnectOptions::default();
    assert_eq!(options.unknown_host, UnknownHostPolicy::Reject);

    let relaxed = SftpConnectOptions::default().accept_unknown_host_approved_by("运维赵六");
    match relaxed.unknown_host {
        UnknownHostPolicy::Accept { approved_by } => assert_eq!(approved_by, "运维赵六"),
        UnknownHostPolicy::Reject => panic!("应当切换为显式接受策略"),
    }
}
