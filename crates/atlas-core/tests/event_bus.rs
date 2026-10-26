//! 事件总线的行为测试。
//!
//! 重点是 `broadcast` 的两个易错语义：
//! 1. 订阅者落后时旧消息被**静默丢弃**，只通过 `Lagged` 告知；
//! 2. 消息在所有活跃接收方取走前不会从通道中移除。
//!
//! `atlas_core::EventBus::recv` 把 `Lagged` 显式化为 `RecvOutcome::Lagged`，
//! 消费方必须自行处理（不能当成正常返回忽略掉）。

use atlas_core::event::{Event, EventBus, LogEvent, LogLevel, RecvOutcome};
use atlas_core::{ServerId, ServerState};

fn server() -> ServerId {
    ServerId::new("survival")
}

fn log_event(msg: &str) -> Event {
    Event::Log(LogEvent {
        server: server(),
        timestamp: Some("12:34:56".into()),
        level: LogLevel::Info,
        logger: Some("minecraft/MinecraftServer".into()),
        thread: Some("Server thread".into()),
        message: msg.to_owned(),
        raw: format!("[12:34:56] [Server thread/INFO]: {msg}"),
    })
}

#[tokio::test]
async fn 订阅者能收到发布的事件() {
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();
    assert!(bus.publish(log_event("hello")));
    match EventBus::recv(&mut rx).await {
        RecvOutcome::Event(Event::Log(log)) => assert_eq!(log.message, "hello"),
        other => panic!("期望收到事件，实际 {other:?}"),
    }
}

#[tokio::test]
async fn 无订阅者时发布返回假但不报错() {
    let bus = EventBus::new(16);
    assert!(
        !bus.publish(log_event("nobody listens")),
        "没有接收者时 publish 应返回 false"
    );
}

#[tokio::test]
async fn 多个订阅者都能收到同一条事件() {
    let bus = EventBus::new(16);
    let mut a = bus.subscribe();
    let mut b = bus.subscribe();
    assert_eq!(bus.receiver_count(), 2);
    bus.publish(log_event("fanout"));
    for rx in [&mut a, &mut b] {
        match EventBus::recv(rx).await {
            RecvOutcome::Event(Event::Log(log)) => assert_eq!(log.message, "fanout"),
            other => panic!("期望收到事件，实际 {other:?}"),
        }
    }
}

#[tokio::test]
async fn 后订阅者不会收到订阅之前的事件() {
    let bus = EventBus::new(16);
    bus.publish(log_event("before"));
    let mut rx = bus.subscribe();
    bus.publish(log_event("after"));
    match EventBus::recv(&mut rx).await {
        RecvOutcome::Event(Event::Log(log)) => assert_eq!(
            log.message, "after",
            "broadcast 不为后加入者回放历史，只会收到订阅后的消息"
        ),
        other => panic!("期望收到事件，实际 {other:?}"),
    }
}

#[tokio::test]
async fn 消费者落后时显式报告_lagged() {
    // 容量 4，连发 10 条，慢消费者必然落后。
    let bus = EventBus::new(4);
    let mut rx = bus.subscribe();
    for i in 0..10 {
        bus.publish(log_event(&format!("line-{i}")));
    }
    // 第一次 recv 要么直接拿到 Lagged，要么拿到一条事件后再 Lagged。
    let mut lagged_seen = false;
    for _ in 0..3 {
        match EventBus::recv(&mut rx).await {
            RecvOutcome::Lagged(n) => {
                assert!(n > 0, "Lagged 应带上丢失条数");
                lagged_seen = true;
                break;
            }
            RecvOutcome::Event(_) => continue,
            RecvOutcome::Closed => panic!("不应关闭"),
        }
    }
    assert!(lagged_seen, "落后消费者必须能观测到 Lagged 信号");
}

#[tokio::test]
async fn 容量为零时回退到默认容量() {
    let bus = EventBus::new(0);
    assert_eq!(bus.capacity(), atlas_core::event::DEFAULT_EVENT_CAPACITY);
    let bus = EventBus::new(32);
    assert_eq!(bus.capacity(), 32);
}

#[tokio::test]
async fn 所有发送端关闭后_recv_返回_closed() {
    let bus = EventBus::new(4);
    let mut rx = bus.subscribe();
    drop(bus);
    match EventBus::recv(&mut rx).await {
        RecvOutcome::Closed => {}
        other => panic!("期望 Closed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn 状态事件可携带服务器状态变化() {
    let bus = EventBus::new(4);
    let mut rx = bus.subscribe();
    bus.publish(Event::ServerState(atlas_core::event::ServerStateEvent {
        server: server(),
        state: ServerState::Running,
        detail: None,
    }));
    match EventBus::recv(&mut rx).await {
        RecvOutcome::Event(Event::ServerState(ev)) => {
            assert_eq!(ev.server, server());
            assert_eq!(ev.state, ServerState::Running);
        }
        other => panic!("期望状态事件，实际 {other:?}"),
    }
}

#[test]
fn 事件可携带所属服务器() {
    let ev = log_event("x");
    assert_eq!(ev.server(), &server());
}
