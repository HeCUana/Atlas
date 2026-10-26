//! 终端命令执行与返回信息采集。
//!
//! ## 结果边界（重要）
//!
//! Minecraft 服务端**没有结构化的命令返回通道**：命令输出最终仍以控制台行
//! 的形式回到 stdout。因此"采集返回信息"在实现上必然是**基于日志行的启发式**。
//!
//! 约定：
//! - 执行命令后，在该服务端的输出流中收集行；
//! - 收集窗口由 `timeout` 决定，超时即返回当前已收集到的内容；
//! - 返回值是 `Option<String>`：`None` 表示在窗口内**没有观察到任何输出**
//!   （区别于"观察到空输出"——后者返回 `Some("")`）。
//! - 采集到的内容是**启发式提取**的，调用方应把它当作"尽力而为的文本"，
//!   而不是权威的结构化结果。

use std::future::Future;
use std::time::Duration;

use atlas_core::{Result, ServerId};
use tokio::sync::broadcast;

/// 控制台命令源：[`ConsoleBridge`] 依赖的最小接口。
///
/// 抽成 trait 的目的：让"执行命令 + 采集返回信息"这一层可以脱离真实子进程做
/// 单元测试（测试用 mock 实现本 trait），同时**不改变** [`crate::ManagedServer`]
/// 的公开 API。
///
/// trait 使用 RPITIT（返回 `impl Future`）而不是 `async fn`，这样 rustdoc 与
/// clippy 都能显式看到 `Send` 约束。
pub trait ConsoleSource: Send {
    /// 订阅**实时**控制台输出行（**不含**历史回放）。
    ///
    /// 命令采集窗口内只应包含命令执行之后产生的行，因此这里必须订阅实时流，
    /// 不能带上历史回放。
    fn subscribe_live_lines(&self) -> broadcast::Receiver<String>;

    /// 向服务端 stdin 投递一条命令。
    fn send_command(&mut self, command: &str) -> impl Future<Output = Result<()>> + Send;

    /// 服务端标识（仅用于日志）。
    fn server_id(&self) -> &ServerId;
}

/// 默认的输出采集窗口。
pub const DEFAULT_CAPTURE_TIMEOUT: Duration = Duration::from_millis(1500);

/// 终端命令执行桥。
#[derive(Debug, Clone)]
pub struct ConsoleBridge {
    timeout: Duration,
}

impl Default for ConsoleBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleBridge {
    /// 创建执行桥，使用默认采集窗口。
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_CAPTURE_TIMEOUT,
        }
    }

    /// 创建执行桥，自定义采集窗口。
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// 当前采集窗口。
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 在指定服务端执行命令并采集返回信息。
    ///
    /// 执行顺序：先订阅输出（避免漏掉命令产生的即时回显），再投递命令，
    /// 然后在 `timeout` 窗口内收集输出行。
    pub async fn execute<S: ConsoleSource>(
        &self,
        server: &mut S,
        command: &str,
    ) -> Result<Option<String>> {
        self.execute_with_timeout(server, command, self.timeout)
            .await
    }

    /// 在指定服务端执行命令并采集返回信息，自定义窗口。
    pub async fn execute_with_timeout<S: ConsoleSource>(
        &self,
        server: &mut S,
        command: &str,
        timeout: Duration,
    ) -> Result<Option<String>> {
        // 先订阅再投递命令，避免漏掉命令产生的即时回显。
        let mut rx = server.subscribe_live_lines();
        server.send_command(command).await?;

        let mut collected: Vec<String> = Vec::new();
        // 记录是否"观察到"输出（哪怕观察到的是空行），用于区分
        // "没有输出"（None）与 "观察到空输出"（Some("")）。
        let mut observed = false;
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(line)) => {
                    observed = true;
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        collected.push(trimmed.to_owned());
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    // 落后不是正常返回，必须显式处理（AGENTS.md 4.2）。
                    tracing::warn!("采集 {} 输出时落后 {n} 行", server.server_id());
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => break,
            }
        }

        if !observed {
            Ok(None)
        } else {
            Ok(Some(collected.join("\n")))
        }
    }

    /// 在指定服务器上执行命令（用于需要跨服务器分发的场景）。
    pub async fn execute_on<S: ConsoleSource>(
        &self,
        server: &mut S,
        command: &str,
        _target: &ServerId,
    ) -> Result<Option<String>> {
        self.execute(server, command).await
    }
}

#[cfg(test)]
mod tests {
    use super::{ConsoleBridge, ConsoleSource};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use atlas_core::{AtlasError, Result, ServerId};
    use tokio::sync::broadcast;

    /// 模拟服务端的控制台源：投递命令后按脚本产出输出行。
    struct MockSource {
        id: ServerId,
        tx: broadcast::Sender<String>,
        /// 投递命令后额外产出的行。
        extra: Vec<String>,
        /// 产出前的延迟（毫秒）。
        delay_ms: u64,
        /// 投递命令是否失败。
        fail: bool,
        /// 已投递的命令（用于断言命令确实写入了 stdin）。
        sent: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl MockSource {
        fn new(capacity: usize) -> Self {
            let (tx, _rx) = broadcast::channel(capacity);
            Self {
                id: ServerId::new("mock"),
                tx,
                extra: Vec::new(),
                delay_ms: 0,
                fail: false,
                sent: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn with_extra(mut self, extra: &[&str]) -> Self {
            self.extra = extra.iter().map(|s| (*s).to_owned()).collect();
            self
        }

        fn with_delay(mut self, delay_ms: u64) -> Self {
            self.delay_ms = delay_ms;
            self
        }

        fn failing() -> Self {
            let mut source = Self::new(16);
            source.fail = true;
            source
        }

        fn sent(&self) -> Vec<String> {
            match self.sent.lock() {
                Ok(guard) => (*guard).clone(),
                Err(poisoned) => (*poisoned.into_inner()).clone(),
            }
        }
    }

    impl ConsoleSource for MockSource {
        fn subscribe_live_lines(&self) -> broadcast::Receiver<String> {
            self.tx.subscribe()
        }

        async fn send_command(&mut self, command: &str) -> Result<()> {
            if self.fail {
                return Err(AtlasError::Process(format!(
                    "向 {} 投递命令失败: 测试注入的错误",
                    self.id
                )));
            }
            if let Ok(mut guard) = self.sent.lock() {
                guard.push(command.to_owned());
            }
            // 模拟子进程在收到命令后产出输出。
            let tx = self.tx.clone();
            let extra = self.extra.clone();
            let delay = Duration::from_millis(self.delay_ms);
            let echo = format!("MOCK-ECHO:{command}");
            tokio::spawn(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let _ = tx.send(echo);
                for line in extra {
                    let _ = tx.send(line);
                }
            });
            Ok(())
        }

        fn server_id(&self) -> &ServerId {
            &self.id
        }
    }

    #[tokio::test]
    async fn captures_immediate_output() {
        let mut source = MockSource::new(16);
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(500));
        let output = bridge
            .execute(&mut source, "list")
            .await
            .expect("执行命令应成功");
        let output = output.expect("应采集到输出");
        assert!(output.contains("MOCK-ECHO:list"), "输出: {output}");
        assert_eq!(source.sent(), vec!["list".to_string()]);
    }

    #[tokio::test]
    async fn returns_none_when_no_output() {
        // 不产出任何行的源：用一个"沉默"的实现。
        struct SilentSource {
            id: ServerId,
            tx: broadcast::Sender<String>,
        }
        impl ConsoleSource for SilentSource {
            fn subscribe_live_lines(&self) -> broadcast::Receiver<String> {
                self.tx.subscribe()
            }
            async fn send_command(&mut self, _command: &str) -> Result<()> {
                Ok(())
            }
            fn server_id(&self) -> &ServerId {
                &self.id
            }
        }
        let (tx, _rx) = broadcast::channel(4);
        let mut source = SilentSource {
            id: ServerId::new("silent"),
            tx,
        };
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(200));
        let output = bridge
            .execute(&mut source, "list")
            .await
            .expect("执行命令应成功");
        assert_eq!(output, None, "没有输出时应返回 None 而不是 Some(\"\")");
    }

    #[tokio::test]
    async fn returns_none_when_output_arrives_after_timeout() {
        let mut source = MockSource::new(16).with_delay(300);
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(80));
        let started = Instant::now();
        let output = bridge
            .execute(&mut source, "slow")
            .await
            .expect("执行命令应成功");
        let elapsed = started.elapsed();
        assert_eq!(output, None, "超时后才出现的输出不应被采集");
        assert!(
            elapsed < Duration::from_millis(300),
            "不应挂起等待，耗时: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn handles_lagged_without_panic() {
        // 容量为 1 的通道：连续投递多行必然让接收方落后。
        let mut source = MockSource::new(1).with_extra(&["EXTRA-1", "EXTRA-2", "EXTRA-3"]);
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(300));
        let result = bridge.execute(&mut source, "flood").await;
        assert!(result.is_ok(), "落后不应导致错误或 panic");
    }

    #[tokio::test]
    async fn empty_line_counts_as_observed_output() {
        struct BlankSource {
            id: ServerId,
            tx: broadcast::Sender<String>,
        }
        impl ConsoleSource for BlankSource {
            fn subscribe_live_lines(&self) -> broadcast::Receiver<String> {
                self.tx.subscribe()
            }
            async fn send_command(&mut self, _command: &str) -> Result<()> {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send("   ".to_string());
                });
                Ok(())
            }
            fn server_id(&self) -> &ServerId {
                &self.id
            }
        }
        let (tx, _rx) = broadcast::channel(4);
        let mut source = BlankSource {
            id: ServerId::new("blank"),
            tx,
        };
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(300));
        let output = bridge
            .execute(&mut source, "list")
            .await
            .expect("执行命令应成功");
        assert_eq!(output, Some(String::new()), "观察到空输出应返回 Some(\"\")");
    }

    #[tokio::test]
    async fn joins_multiple_lines() {
        let mut source = MockSource::new(16).with_extra(&["LINE-A", "LINE-B"]);
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(500));
        let output = bridge
            .execute(&mut source, "list")
            .await
            .expect("执行命令应成功")
            .expect("应采集到输出");
        assert!(output.contains("LINE-A"), "输出: {output}");
        assert!(output.contains("LINE-B"), "输出: {output}");
        assert!(output.contains('\n'), "多行应按换行拼接: {output}");
    }

    #[tokio::test]
    async fn propagates_send_failure() {
        let mut source = MockSource::failing();
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(100));
        let result = bridge.execute(&mut source, "list").await;
        assert!(matches!(result, Err(AtlasError::Process(_))));
    }

    #[tokio::test]
    async fn custom_timeout_is_used() {
        let bridge = ConsoleBridge::with_timeout(Duration::from_millis(777));
        assert_eq!(bridge.timeout(), Duration::from_millis(777));
        assert_eq!(
            ConsoleBridge::new().timeout(),
            super::DEFAULT_CAPTURE_TIMEOUT
        );
    }
}
