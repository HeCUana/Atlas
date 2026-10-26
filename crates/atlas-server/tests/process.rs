//! `ManagedServer` 集成测试：用**真实子进程**验证生命周期与输出分发。

use std::time::Duration;

use atlas_core::ServerState;
use atlas_server::ManagedServer;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

mod common;

use common::{BULK_LINES, MODE_BULK, MODE_EXIT, MODE_FLOOD, MODE_SERVE, helper_config};

/// 单个测试的等待上限。
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

/// 在超时内等待包含 `needle` 的输出行，返回该行。
async fn wait_for_line(rx: &mut broadcast::Receiver<String>, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等待输出行 {needle} 超时");
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(line)) => {
                if line.contains(needle) {
                    return line;
                }
            }
            // 落后会丢消息，但不影响"是否出现过目标行"的判断。
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => panic!("输出流已关闭"),
            Err(_) => panic!("等待输出行 {needle} 超时"),
        }
    }
}

/// 在超时内等待**所有**指定的输出行（顺序无关）。
async fn wait_for_lines(rx: &mut broadcast::Receiver<String>, needles: &[&str]) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    let mut found: Vec<String> = Vec::new();
    while found.len() < needles.len() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "等待输出行 {needles:?} 超时，已找到: {found:?}"
        );
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(line)) => {
                for needle in needles {
                    if line.contains(needle) && !found.iter().any(|f| f == needle) {
                        found.push((*needle).to_owned());
                    }
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => panic!("输出流已关闭"),
            Err(_) => panic!("等待输出行 {needles:?} 超时"),
        }
    }
    found
}

/// 在时间窗内收集收到的所有行。
async fn collect_lines(rx: &mut broadcast::Receiver<String>, window: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + window;
    let mut lines = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(line)) => lines.push(line),
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => break,
        }
    }
    lines
}

#[tokio::test]
async fn spawn_delivers_stdout_lines() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut rx = server.subscribe_lines();
    // 订阅之后再触发输出：`spawn` 返回前子进程可能已经打印过启动横幅，
    // 而广播通道不会为后加入的订阅者回放历史消息。
    server.send_command("hello").await.expect("投递命令应成功");
    let line = wait_for_line(&mut rx, "ATLAS-TEST-ECHO:hello").await;
    assert_eq!(line.trim(), "ATLAS-TEST-ECHO:hello");
}

#[tokio::test]
async fn send_command_reaches_child_stdin() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut rx = server.subscribe_lines();
    server
        .send_command("say hello world")
        .await
        .expect("投递命令应成功");
    let line = wait_for_line(&mut rx, "ATLAS-TEST-ECHO:say hello world").await;
    assert!(
        line.contains("say hello world"),
        "子进程应回显收到的命令，实际: {line}"
    );
}

#[tokio::test]
async fn subscriber_receives_stdout_and_stderr_lines() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut rx = server.subscribe_lines();
    // 两条命令分别触发 stdout 与 stderr 的输出。
    server.send_command("hello").await.expect("投递命令应成功");
    server.send_command("stderr").await.expect("投递命令应成功");
    let found = wait_for_lines(
        &mut rx,
        &["ATLAS-TEST-ECHO:hello", "ATLAS-TEST-STDERR-LINE"],
    )
    .await;
    assert_eq!(found.len(), 2, "stdout 与 stderr 两侧的行都应到达订阅者");
}

#[tokio::test]
async fn concurrent_reading_avoids_pipe_deadlock() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_FLOOD, 0))
        .await
        .expect("启动子进程应成功");
    let mut rx = server.subscribe_lines();
    // 子进程往两侧各写约 400KB，远超管道缓冲区：只有两侧都被并发读取时，
    // 它才能写完并退出（结束标记才可能出现）。
    let found = wait_for_lines(
        &mut rx,
        &["ATLAS-TEST-FLOOD-OUT-DONE", "ATLAS-TEST-FLOOD-ERR-DONE"],
    )
    .await;
    assert_eq!(found.len(), 2, "两侧的输出都应被完整读取: {found:?}");

    // 子进程能自然退出，说明它没有被写满的管道卡住。
    let code = server.wait().await.expect("等待退出应成功");
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn stop_terminates_child_and_updates_state() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    assert_eq!(server.state(), ServerState::Starting);

    server
        .stop(Duration::from_secs(15))
        .await
        .expect("停止应成功");
    assert_eq!(server.state(), ServerState::Stopped);
}

#[tokio::test]
async fn stop_is_idempotent() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    server
        .stop(Duration::from_secs(15))
        .await
        .expect("第一次停止应成功");
    server
        .stop(Duration::from_secs(15))
        .await
        .expect("重复停止应成功");
    assert_eq!(server.state(), ServerState::Stopped);
}

#[tokio::test]
async fn wait_returns_exit_code() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_EXIT, 7))
        .await
        .expect("启动子进程应成功");
    let code = server.wait().await.expect("等待退出应成功");
    assert_eq!(code, Some(7));
    assert_eq!(server.state(), ServerState::Stopped);
}

#[tokio::test]
async fn wait_on_missing_child_returns_none() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_EXIT, 0))
        .await
        .expect("启动子进程应成功");
    let _ = server.wait().await.expect("等待退出应成功");
    // 子进程已经收割过，第二次 wait 不应再报错。
    let code = server.wait().await.expect("重复等待应成功");
    assert_eq!(code, None);
}

#[tokio::test]
async fn spawn_fails_for_unknown_command() {
    let mut config = helper_config("s1", MODE_SERVE, 0);
    config.command = "atlas-不存在的可执行文件-xyz".to_owned();
    let result = ManagedServer::spawn(config).await;
    assert!(result.is_err(), "启动不存在的命令应失败");
    let err = result.err().expect("应返回错误");
    assert!(
        matches!(err, atlas_core::AtlasError::Process(_)),
        "错误类型应为 Process，实际: {err:?}"
    );
}

#[tokio::test]
async fn send_command_fails_after_child_exited() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_EXIT, 0))
        .await
        .expect("启动子进程应成功");
    let _ = server.wait().await.expect("等待退出应成功");
    let result = server.send_command("hello").await;
    assert!(
        result.is_err(),
        "子进程退出后投递命令应失败，实际: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// 回放缓冲：后加入的订阅者也能拿到"订阅之前"产生的行。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn late_subscriber_receives_previous_lines() {
    let server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");

    // 先订阅一次，等到子进程的启动横幅进入实时流——此时它也已进入回放缓冲。
    let mut first = server.subscribe_lines();
    wait_for_line(&mut first, "ATLAS-TEST-STDOUT-READY").await;

    // 后加入的订阅者必须能从回放缓冲里拿到这条"订阅之前"产生的行。
    let mut late = server.subscribe_lines();
    let line = wait_for_line(&mut late, "ATLAS-TEST-STDOUT-READY").await;
    assert!(
        line.contains("ATLAS-TEST-STDOUT-READY"),
        "后订阅者应收到订阅之前产生的行，实际: {line}"
    );
}

#[tokio::test]
async fn replay_does_not_duplicate_lines() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut first = server.subscribe_lines();
    wait_for_line(&mut first, "ATLAS-TEST-STDOUT-READY").await;

    let mut second = server.subscribe_lines();
    server.send_command("hello").await.expect("投递命令应成功");

    // 回放与实时流在交界处不能把同一行算两次。
    let lines = collect_lines(&mut second, Duration::from_millis(800)).await;
    let count = lines
        .iter()
        .filter(|line| line.contains("ATLAS-TEST-STDOUT-READY"))
        .count();
    assert_eq!(count, 1, "同一行不应被回放与实时流重复投递: {lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("ATLAS-TEST-ECHO:hello")),
        "订阅之后产生的实时行也应收到: {lines:?}"
    );
}

#[tokio::test]
async fn backlog_is_bounded_and_evicts_old_lines() {
    let server = ManagedServer::spawn(helper_config("s1", MODE_BULK, 0))
        .await
        .expect("启动子进程应成功");
    let mut first = server.subscribe_lines();
    // 等子进程把全部行写完（此时它们都已进入回放缓冲）。
    wait_for_line(&mut first, "ATLAS-TEST-BULK-DONE").await;

    let mut second = server.subscribe_lines();
    let lines = collect_lines(&mut second, Duration::from_millis(800)).await;

    assert!(
        lines.len() <= atlas_server::BACKLOG_CAPACITY,
        "回放的行数不应超过缓冲容量: {}",
        lines.len()
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("ATLAS-TEST-BULK-0-END")),
        "超出容量的早期行应被淘汰"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&format!("ATLAS-TEST-BULK-{}-END", BULK_LINES - 1))),
        "最新的行应保留在缓冲里"
    );
}

#[tokio::test]
async fn multiple_subscribers_each_get_backlog_and_live_lines() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut first = server.subscribe_lines();
    wait_for_line(&mut first, "ATLAS-TEST-STDOUT-READY").await;

    let mut a = server.subscribe_lines();
    let mut b = server.subscribe_lines();

    // 两个后订阅者都能拿到"订阅之前"的历史行。
    wait_for_line(&mut a, "ATLAS-TEST-STDOUT-READY").await;
    wait_for_line(&mut b, "ATLAS-TEST-STDOUT-READY").await;

    // 订阅之后产生的实时行也都能收到。
    server.send_command("hello").await.expect("投递命令应成功");
    wait_for_line(&mut a, "ATLAS-TEST-ECHO:hello").await;
    wait_for_line(&mut b, "ATLAS-TEST-ECHO:hello").await;
}

#[tokio::test]
async fn live_subscription_does_not_replay_history() {
    let mut server = ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0))
        .await
        .expect("启动子进程应成功");
    let mut first = server.subscribe_lines();
    wait_for_line(&mut first, "ATLAS-TEST-STDOUT-READY").await;

    // 实时订阅不回放历史行：命令采集必须只拿到命令之后产生的行。
    let mut live = server.subscribe_live_lines();
    server.send_command("hello").await.expect("投递命令应成功");
    let lines = collect_lines(&mut live, Duration::from_millis(800)).await;

    assert!(
        !lines
            .iter()
            .any(|line| line.contains("ATLAS-TEST-STDOUT-READY")),
        "实时订阅不应回放历史行: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("ATLAS-TEST-ECHO:hello")),
        "实时订阅应收到新产生的行: {lines:?}"
    );
}

/// 没有 Tokio 运行时时，退化为只订阅实时流，且不得 panic。
#[test]
fn subscribe_without_runtime_falls_back_to_live_stream() {
    let runtime = tokio::runtime::Runtime::new().expect("创建运行时应成功");
    let mut server = runtime
        .block_on(ManagedServer::spawn(helper_config("s1", MODE_SERVE, 0)))
        .expect("启动子进程应成功");

    // 在运行时之外订阅：`Handle::try_current()` 失败，应退化为实时流。
    let mut rx = server.subscribe_lines();

    runtime.block_on(async {
        server.send_command("hello").await.expect("投递命令应成功");
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "等待输出行超时");
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(line)) => {
                    if line.contains("ATLAS-TEST-ECHO:hello") {
                        return;
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(RecvError::Closed)) => panic!("输出流已关闭"),
                Err(_) => panic!("等待输出行超时"),
            }
        }
    });
}
