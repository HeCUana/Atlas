//! `Supervisor` 集成测试：批量开关服、状态事件与错误语义。

use std::time::Duration;

use atlas_core::{
    AtlasConfig, AtlasError, Event, EventBus, ServerId, ServerState, ServerStateEvent,
};
use atlas_server::{ConsoleBridge, Supervisor};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

mod common;

use common::{MODE_EXIT, MODE_SERVE, helper_config};

/// 单个测试的等待上限。
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

/// 构造只包含辅助子进程的配置。
fn config_with(servers: &[(&str, bool)]) -> AtlasConfig {
    AtlasConfig {
        servers: servers
            .iter()
            .map(|(id, auto_start)| {
                let mut cfg = helper_config(id, MODE_SERVE, 0);
                cfg.auto_start = *auto_start;
                cfg
            })
            .collect(),
        ..AtlasConfig::default()
    }
}

/// 等待下一个服务器状态事件（跳过其它事件）。
async fn next_state_event(rx: &mut broadcast::Receiver<Event>) -> ServerStateEvent {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等待状态事件超时");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(Event::ServerState(event))) => return event,
            Ok(Ok(_)) => continue,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => panic!("事件总线已关闭"),
            Err(_) => panic!("等待状态事件超时"),
        }
    }
}

/// 断言在给定时间窗内**没有**新的事件到达。
async fn assert_no_event(rx: &mut broadcast::Receiver<Event>) {
    match tokio::time::timeout(Duration::from_millis(400), rx.recv()).await {
        Err(_) => {}
        Ok(Ok(event)) => panic!("不应有事件到达，实际: {event:?}"),
        Ok(Err(RecvError::Lagged(_))) => {}
        Ok(Err(RecvError::Closed)) => panic!("事件总线已关闭"),
    }
}

#[tokio::test]
async fn start_publishes_running_state() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("启动应成功");

    let event = next_state_event(&mut rx).await;
    assert_eq!(event.server, ServerId::new("s1"));
    assert_eq!(event.state, ServerState::Running);

    supervisor.stop("s1").await.expect("停止应成功");
}

#[tokio::test]
async fn starting_twice_does_not_spawn_again() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("第一次启动应成功");
    supervisor.start("s1").await.expect("重复启动不应报错");

    let event = next_state_event(&mut rx).await;
    assert_eq!(event.state, ServerState::Running);
    // 第二次启动不应再拉起进程，也不应再发布状态事件。
    assert_no_event(&mut rx).await;

    supervisor.stop("s1").await.expect("停止应成功");
}

#[tokio::test]
async fn stop_publishes_stopping_and_stopped() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("启动应成功");
    let running = next_state_event(&mut rx).await;
    assert_eq!(running.state, ServerState::Running);

    supervisor.stop("s1").await.expect("停止应成功");

    let stopping = next_state_event(&mut rx).await;
    assert_eq!(stopping.state, ServerState::Stopping);
    let stopped = next_state_event(&mut rx).await;
    assert_eq!(stopped.state, ServerState::Stopped);
}

#[tokio::test]
async fn restart_republishes_state() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("启动应成功");
    let running = next_state_event(&mut rx).await;
    assert_eq!(running.state, ServerState::Running);

    supervisor.restart("s1").await.expect("重启应成功");

    let stopping = next_state_event(&mut rx).await;
    assert_eq!(stopping.state, ServerState::Stopping);
    let stopped = next_state_event(&mut rx).await;
    assert_eq!(stopped.state, ServerState::Stopped);
    let running_again = next_state_event(&mut rx).await;
    assert_eq!(running_again.state, ServerState::Running);

    supervisor.stop("s1").await.expect("停止应成功");
}

#[tokio::test]
async fn unknown_server_returns_unknown_server_error() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus);

    let err = supervisor.start("nope").await.expect_err("应报错");
    assert!(matches!(err, AtlasError::UnknownServer(ref id) if id == "nope"));

    let err = supervisor.stop("nope").await.expect_err("应报错");
    assert!(matches!(err, AtlasError::UnknownServer(ref id) if id == "nope"));

    let err = supervisor.restart("nope").await.expect_err("应报错");
    assert!(matches!(err, AtlasError::UnknownServer(ref id) if id == "nope"));
}

#[tokio::test]
async fn 配置里存在但未托管的服务器应报_not_managed() {
    // s1 在配置里，但本进程没有启动它 —— 属于"不由本进程托管"，
    // 与"配置里压根没有这台服务器"是两种不同的问题，错误类型必须区分开。
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus);

    let err = supervisor.stop("s1").await.expect_err("应报错");
    assert!(
        matches!(err, AtlasError::NotManaged(ref id) if id == "s1"),
        "期望 NotManaged，实际 {err:?}"
    );
}

#[tokio::test]
async fn 子进程意外退出应被轮询察觉并标记为失败() {
    let mut config = AtlasConfig::default();
    config.servers.push(helper_config("boom", MODE_EXIT, 3));
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus);

    supervisor.start("boom").await.expect("启动应成功");

    // 等子进程真正退出，再轮询。
    let mut changes = Vec::new();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        changes = supervisor.poll_changes();
        if !changes.is_empty() {
            break;
        }
    }

    assert_eq!(changes.len(), 1, "应恰好检测到一次退出，实际 {changes:?}");
    let (id, state) = &changes[0];
    assert_eq!(id.as_str(), "boom");
    assert_eq!(*state, ServerState::Failed, "非零退出码应标记为 Failed");

    // 同时应发出状态事件。注意 start 时已经发过一条 `Running`，
    // 所以这里要一直读到 `Failed` 那条为止。
    let mut failed_event = None;
    for _ in 0..20 {
        let event = next_state_event(&mut rx).await;
        if event.state == ServerState::Failed {
            failed_event = Some(event);
            break;
        }
    }
    let event = failed_event.expect("应收到状态为 Failed 的事件");
    assert_eq!(event.server.as_str(), "boom");
    assert!(
        event.detail.unwrap_or_default().contains("3"),
        "事件详情应带上退出码"
    );
}

#[tokio::test]
async fn 仍在运行的子进程不应被轮询误报() {
    let mut config = AtlasConfig::default();
    config.servers.push(helper_config("alive", MODE_SERVE, 0));
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus);

    supervisor.start("alive").await.expect("启动应成功");
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        supervisor.poll_changes().is_empty(),
        "运行中的服务器不应被判定为退出"
    );
}

#[tokio::test]
async fn start_all_only_starts_auto_start_servers() {
    let config = config_with(&[("s1", true), ("s2", false)]);
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start_all().await.expect("批量启动应成功");

    let event = next_state_event(&mut rx).await;
    assert_eq!(event.server, ServerId::new("s1"));
    assert_eq!(event.state, ServerState::Running);
    assert_no_event(&mut rx).await;

    let states = supervisor.states();
    let s1 = states
        .iter()
        .find(|(id, _)| id == &ServerId::new("s1"))
        .map(|(_, state)| *state)
        .expect("应包含 s1");
    let s2 = states
        .iter()
        .find(|(id, _)| id == &ServerId::new("s2"))
        .map(|(_, state)| *state)
        .expect("应包含 s2");
    assert_eq!(s1, ServerState::Running);
    assert_eq!(s2, ServerState::Stopped);

    supervisor.stop_all().await;
}

#[tokio::test]
async fn stop_all_stops_every_server() {
    let config = config_with(&[("s1", true), ("s2", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start_all().await.expect("批量启动应成功");
    let errors = supervisor.stop_all().await;
    assert!(errors.is_empty(), "停止不应有错误: {errors:?}");

    let states = supervisor.states();
    assert!(
        states
            .iter()
            .all(|(_, state)| *state == ServerState::Stopped),
        "全部服务器应处于已停止状态: {states:?}"
    );
}

#[tokio::test]
async fn restart_all_restarts_running_servers() {
    let config = config_with(&[("s1", true), ("s2", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start_all().await.expect("批量启动应成功");
    supervisor.restart_all().await.expect("批量重启应成功");

    let states = supervisor.states();
    assert!(
        states
            .iter()
            .all(|(_, state)| *state == ServerState::Running),
        "全部服务器应处于运行状态: {states:?}"
    );

    supervisor.stop_all().await;
}

#[tokio::test]
async fn subscribe_lines_exposes_server_output() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("启动应成功");
    let mut rx = supervisor
        .subscribe_lines("s1")
        .expect("已启动的服务器应可订阅");

    let server = supervisor.server_mut("s1").expect("应能取得服务器引用");
    server.send_command("hello").await.expect("投递命令应成功");

    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    let mut found = false;
    while !found {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等待输出行超时");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(line)) => {
                if line.contains("ATLAS-TEST-ECHO:hello") {
                    found = true;
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => panic!("输出流已关闭"),
            Err(_) => panic!("等待输出行超时"),
        }
    }
    assert!(found, "应收到子进程的回显");

    supervisor.stop("s1").await.expect("停止应成功");
}

#[tokio::test]
async fn console_bridge_captures_output_from_real_server() {
    let config = config_with(&[("s1", true)]);
    let bus = EventBus::new(64);
    let mut supervisor = Supervisor::new(config, bus.clone());

    supervisor.start("s1").await.expect("启动应成功");
    let server = supervisor.server_mut("s1").expect("应能取得服务器引用");

    let bridge = ConsoleBridge::with_timeout(Duration::from_millis(1500));
    let output = bridge
        .execute(server, "hello")
        .await
        .expect("执行命令应成功")
        .expect("应采集到输出");
    assert!(
        output.contains("ATLAS-TEST-ECHO:hello"),
        "采集到的输出应包含子进程回显: {output}"
    );

    supervisor.stop("s1").await.expect("停止应成功");
}
