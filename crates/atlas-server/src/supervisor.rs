//! 多服务器监督器：统一管理所有被托管的服务端进程。

use std::collections::HashMap;
use std::time::Duration;

use atlas_core::{AtlasConfig, Event, EventBus, Result, ServerId, ServerState, ServerStateEvent};
use tokio::sync::broadcast;

use crate::process::ManagedServer;

/// 默认的优雅停止超时。
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// 监督器：持有全部被托管的服务端进程。
pub struct Supervisor {
    config: AtlasConfig,
    bus: EventBus,
    servers: HashMap<ServerId, ManagedServer>,
    stop_timeout: Duration,
}

impl Supervisor {
    /// 创建监督器。
    pub fn new(config: AtlasConfig, bus: EventBus) -> Self {
        Self {
            config,
            bus,
            servers: HashMap::new(),
            stop_timeout: DEFAULT_STOP_TIMEOUT,
        }
    }

    /// 创建监督器，自定义优雅停止超时。
    pub fn with_stop_timeout(mut self, timeout: Duration) -> Self {
        self.stop_timeout = timeout;
        self
    }

    /// 事件总线引用。
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// 全局配置引用。
    pub fn config(&self) -> &AtlasConfig {
        &self.config
    }

    /// 启动指定服务器。
    ///
    /// 同一台服务器重复启动**不会**重复拉起进程（幂等）。
    pub async fn start(&mut self, id: &str) -> Result<()> {
        if self.servers.contains_key(&ServerId::new(id)) {
            return Ok(());
        }
        let cfg = self
            .config
            .server(id)
            .ok_or_else(|| atlas_core::AtlasError::UnknownServer(id.to_owned()))?
            .clone();

        let mut managed = ManagedServer::spawn(cfg).await?;
        managed.set_state(ServerState::Running);
        let server_id = managed.id().clone();
        self.servers.insert(server_id.clone(), managed);
        self.publish_state(&server_id, ServerState::Running, None);
        Ok(())
    }

    /// 停止指定服务器。
    pub async fn stop(&mut self, id: &str) -> Result<()> {
        let key = ServerId::new(id);
        let Some(mut managed) = self.servers.remove(&key) else {
            return Err(atlas_core::AtlasError::UnknownServer(id.to_owned()));
        };
        self.publish_state(&key, ServerState::Stopping, None);
        let result = managed.stop(self.stop_timeout).await;
        self.publish_state(&key, ServerState::Stopped, None);
        result
    }

    /// 重启指定服务器。
    ///
    /// 未在运行的服务器直接启动；已在运行的服务器先停止再启动。
    pub async fn restart(&mut self, id: &str) -> Result<()> {
        if self.servers.contains_key(&ServerId::new(id)) {
            // 停止失败（例如等待退出超时）时向上传播，不静默吞掉。
            self.stop(id).await?;
        }
        self.start(id).await
    }

    /// 启动全部标记了 `auto_start` 的服务器。
    pub async fn start_all(&mut self) -> Result<()> {
        let ids: Vec<String> = self
            .config
            .servers
            .iter()
            .filter(|s| s.auto_start)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.start(&id).await?;
        }
        Ok(())
    }

    /// 停止全部服务器（逐个停止，失败不影响其余）。
    pub async fn stop_all(&mut self) -> Vec<(String, atlas_core::AtlasError)> {
        let ids: Vec<String> = self.servers.keys().map(|k| k.to_string()).collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(e) = self.stop(&id).await {
                errors.push((id, e));
            }
        }
        errors
    }

    /// 重启全部服务器。
    pub async fn restart_all(&mut self) -> Result<()> {
        let ids: Vec<String> = self.servers.keys().map(|k| k.to_string()).collect();
        for id in ids {
            self.restart(&id).await?;
        }
        Ok(())
    }

    /// 订阅某台服务器的控制台输出行。
    ///
    /// 与 [`ManagedServer::subscribe_lines`] 一致：订阅者会**先收到最近的历史行**
    /// （回放缓冲，见 [`crate::BACKLOG_CAPACITY`]），再收到实时行。
    pub fn subscribe_lines(&self, id: &str) -> Option<broadcast::Receiver<String>> {
        self.servers
            .get(&ServerId::new(id))
            .map(|s| s.subscribe_lines())
    }

    /// 取得某台服务器的可变引用（用于执行终端命令）。
    pub fn server_mut(&mut self, id: &str) -> Option<&mut ManagedServer> {
        self.servers.get_mut(&ServerId::new(id))
    }

    /// 所有服务器 ID（按配置顺序）。
    pub fn server_ids(&self) -> Vec<ServerId> {
        self.config.server_ids()
    }

    /// 各服务器当前状态。
    pub fn states(&self) -> Vec<(ServerId, ServerState)> {
        self.config
            .servers
            .iter()
            .map(|s| {
                let id = ServerId::new(s.id.clone());
                let state = self
                    .servers
                    .get(&id)
                    .map(|m| m.state())
                    .unwrap_or(ServerState::Stopped);
                (id, state)
            })
            .collect()
    }

    /// 发布一个服务器状态事件。
    pub fn publish_state(&self, server: &ServerId, state: ServerState, detail: Option<String>) {
        self.bus.publish(Event::ServerState(ServerStateEvent {
            server: server.clone(),
            state,
            detail,
        }));
    }
}
