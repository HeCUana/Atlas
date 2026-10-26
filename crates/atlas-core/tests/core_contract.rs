//! `atlas-core` 的单元测试。
//!
//! 重点覆盖两块安全/契约关键逻辑：
//! - [`crate::perm::PermissionSet`]：三元组授权模型（默认拒绝、通配语义、拒绝路径）；
//! - [`crate::command::CommandTree`]：命令注册冲突检测与参数校验。
//!
//! 事件总线部分单独放在 `tests/` 下的集成测试里。

use atlas_core::command::{CommandSource, CommandSpec, CommandTree};
use atlas_core::error::AtlasError;
use atlas_core::perm::{Action, PermissionSet, Resource, Subject};
use atlas_core::server::ServerId;

fn server() -> ServerId {
    ServerId::new("survival")
}

fn other_server() -> ServerId {
    ServerId::new("creative")
}

// ---------------------------------------------------------------- 权限模型

#[test]
fn 默认拒绝任何未授权的组合() {
    let perms = PermissionSet::new();
    let player = Subject::player("Alice");
    assert!(
        !perms.allows(&player, &Action::ServerStart, &Resource::Server(server())),
        "未授予任何权限时必须拒绝"
    );
}

#[test]
fn require_在未授权时返回权限错误且信息可读() {
    let perms = PermissionSet::new();
    let player = Subject::player("Alice");
    let err = perms
        .require(&player, &Action::ServerGrantOp, &Resource::Server(server()))
        .expect_err("未授权必须报错");
    match err {
        AtlasError::PermissionDenied {
            subject,
            action,
            resource,
        } => {
            assert_eq!(subject, "player:Alice");
            assert_eq!(action, "server.grant_op");
            assert_eq!(resource, "server:survival");
        }
        other => panic!("期望 PermissionDenied，实际得到 {other:?}"),
    }
}

#[test]
fn 授权后可放行对应动作() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerGrantOp],
        [Resource::Server(server())],
    );
    let player = Subject::player("Alice");
    assert!(perms.allows(&player, &Action::ServerGrantOp, &Resource::Server(server())));
}

#[test]
fn 动作不匹配时拒绝() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerGrantOp],
        [Resource::Server(server())],
    );
    let player = Subject::player("Alice");
    assert!(
        !perms.allows(&player, &Action::ServerStop, &Resource::Server(server())),
        "只授予 grant_op 时不能执行 stop"
    );
}

#[test]
fn 按服授权不会跨服生效() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerGrantOp],
        [Resource::Server(server())],
    );
    let player = Subject::player("Alice");
    assert!(
        !perms.allows(
            &player,
            &Action::ServerGrantOp,
            &Resource::Server(other_server())
        ),
        "对 survival 的授权不应作用于 creative"
    );
}

#[test]
fn 主体不同则不共享权限() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerGrantOp],
        [Resource::Server(server())],
    );
    let bob = Subject::player("Bob");
    assert!(!perms.allows(&bob, &Action::ServerGrantOp, &Resource::Server(server())));
}

#[test]
fn 通配动作覆盖全部动作() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::operator("admin"),
        [Action::All],
        [Resource::Server(server())],
    );
    let admin = Subject::operator("admin");
    assert!(perms.allows(&admin, &Action::ServerStart, &Resource::Server(server())));
    assert!(perms.allows(&admin, &Action::ServerStop, &Resource::Server(server())));
    assert!(perms.allows(&admin, &Action::FileDelete, &Resource::Server(server())));
}

#[test]
fn 通配资源覆盖该服务器上的文件() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::operator("admin"),
        [Action::FileRead],
        [Resource::Server(server())],
    );
    let admin = Subject::operator("admin");
    let file = Resource::File {
        server: server(),
        path: "logs/latest.log".into(),
    };
    assert!(perms.allows(&admin, &Action::FileRead, &file));
}

#[test]
fn 全部资源通配覆盖任意资源() {
    let mut perms = PermissionSet::new();
    perms.allow(Subject::operator("admin"), [Action::All], [Resource::All]);
    let admin = Subject::operator("admin");
    assert!(perms.allows(&admin, &Action::PluginManage, &Resource::Plugin("x".into())));
    assert!(perms.allows(
        &admin,
        &Action::FileRead,
        &Resource::File {
            server: other_server(),
            path: "any".into()
        }
    ));
}

#[test]
fn 动作与资源的稳定标识可往返() {
    for action in [
        Action::ServerStart,
        Action::ServerStop,
        Action::ServerRestart,
        Action::ServerExecute,
        Action::ServerGrantOp,
        Action::ServerBackup,
        Action::FileList,
        Action::FileRead,
        Action::FileWrite,
        Action::FileDelete,
        Action::PluginManage,
        Action::ConsoleAttach,
        Action::DaemonControl,
    ] {
        let id = action.as_str();
        let parsed = Action::parse(id).unwrap_or_else(|| panic!("{id} 应可反解析"));
        assert_eq!(parsed, action);
    }
    assert_eq!(Action::parse("不存在的动作"), None);
    assert_eq!(Action::parse("all"), Some(Action::All));
}

#[test]
fn actions_of_汇总某主体的全部动作() {
    let mut perms = PermissionSet::new();
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerStart, Action::ServerStop],
        [Resource::All],
    );
    perms.allow(
        Subject::player("Alice"),
        [Action::ServerStart],
        [Resource::All],
    );
    let actions = perms.actions_of(&Subject::player("Alice"));
    assert!(actions.contains(&Action::ServerStart));
    assert!(actions.contains(&Action::ServerStop));
    assert_eq!(actions.len(), 2, "重复的动作应去重");
}

// ---------------------------------------------------------------- 命令树

fn spec(name: &str) -> CommandSpec {
    CommandSpec::new(name, CommandSource::ServerConsole { server: server() })
}

#[test]
fn 注册与解析命令() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop").with_about("停止服务器", "stop <server>"))
        .expect("注册应成功");
    let invocation = tree.resolve("stop survival").expect("应能解析");
    assert_eq!(invocation.name, "stop");
    assert_eq!(invocation.args, vec!["survival".to_owned()]);
    assert_eq!(invocation.raw, "stop survival");
}

#[test]
fn 命令名冲突时注册失败() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop")).expect("首次注册应成功");
    let err = tree.register(spec("stop")).expect_err("重名应报错");
    assert!(matches!(err, AtlasError::Command(_)));
}

#[test]
fn 别名冲突时注册失败() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop").with_aliases(["halt"]))
        .expect("首次注册应成功");
    let err = tree
        .register(spec("other").with_aliases(["halt"]))
        .expect_err("别名冲突应报错");
    assert!(matches!(err, AtlasError::Command(_)));
}

#[test]
fn 空命令名注册失败() {
    let mut tree = CommandTree::new();
    assert!(tree.register(spec("  ")).is_err());
}

#[test]
fn 未知命令解析失败() {
    let tree = CommandTree::new();
    assert!(tree.resolve("nope").is_err());
    assert!(tree.resolve("   ").is_err());
}

#[test]
fn 参数个数不足或超量时解析失败() {
    let mut tree = CommandTree::new();
    let mut s = spec("op");
    s.min_args = 1;
    s.max_args = Some(2);
    tree.register(s).expect("注册应成功");

    assert!(tree.resolve("op").is_err(), "缺参数应失败");
    assert!(tree.resolve("op a").is_ok());
    assert!(tree.resolve("op a b").is_ok());
    assert!(tree.resolve("op a b c").is_err(), "超量应失败");
}

#[test]
fn 别名可以解析到同一命令() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop").with_aliases(["halt"]))
        .expect("注册应成功");
    let invocation = tree.resolve("halt now").expect("别名应可解析");
    assert_eq!(invocation.name, "stop");
}

#[test]
fn 注销后命令不可再解析() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop")).expect("注册应成功");
    tree.register(spec("op")).expect("注册应成功");
    assert!(tree.unregister("stop"));
    assert!(tree.resolve("stop").is_err());
    assert!(tree.resolve("op").is_ok());
    assert_eq!(tree.len(), 1);
    assert!(!tree.unregister("不存在的命令"));
}

#[test]
fn 注销别名也能移除命令() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop").with_aliases(["halt"]))
        .expect("注册应成功");
    assert!(tree.unregister("halt"));
    assert!(tree.resolve("stop").is_err());
    assert!(tree.resolve("halt").is_err());
}

#[test]
fn 可以渲染为_clap_命令() {
    let mut tree = CommandTree::new();
    tree.register(spec("stop").with_about("停止", "stop <server>"))
        .expect("注册应成功");
    let clap_cmd = tree.to_clap();
    assert!(clap_cmd.get_subcommands().any(|s| s.get_name() == "stop"));
}
