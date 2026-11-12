//! JSONL 事件流落盘：后台订阅 EventBus，逐行追加到 `<base>/events/YYYY-MM-DD.jsonl`
//!
//! ## 文件布局
//!
//! 按日分文件（UTC 日期命名）：单文件体积可控、按天查询/导出无需扫描全量；
//! 文件名取事件的 **UTC** 日期，与行内 `timestamp`（UTC RFC3339）同口径，
//! 文件边界即时间戳日期边界。与 `logs/`（tracing-appender 本地日期）不同是
//! 有意为之：事件天然携带 UTC 时间戳，落盘边界跟着时间戳走，消费方按时间戳
//! 过滤时不会在文件边界处踩到"同一时刻分属两个文件"的歧义。
//!
//! ## 轮转与保留
//!
//! 固定保留 30 天（[`EVENT_RETENTION_DAYS`]，与登录历史口径一致）：启动时清理
//! 一次，此后复用 container 的每日 housekeeping（`spawn_log_cleanup_task`，日志
//! 轮转在用）调用 [`cleanup_expired_event_files`] 兜底——不在 sink 任务内自跑
//! 每日 tick，避免两处各养一个定时器。
//!
//! ## 可靠性口径
//!
//! - 写入失败（磁盘满 / 权限等）只 warn、丢弃该条并待下次事件重开文件，
//!   **不 panic、不阻断事件总线内存侧**——落盘是尽力而为，内存分发才是主干。
//! - 停止时先 `try_recv` 排空积压再 flush 已缓冲行（ServiceHandle 统一模式，
//!   `stop_tx` watch + 应用关闭令牌双驱动，任一先到即进入排空退出）。
//! - 脱敏红线：事件结构本身已无敏感字段（见 [`super`] 模块 doc），写入层
//!   **不做二次脱敏**——该信任链由事件类型的白名单式载荷设计保证。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::EventEnvelope;

/// 事件流保留天数（固定 30 天，与登录历史 `HISTORY_RETENTION_DAYS` 同口径）
pub const EVENT_RETENTION_DAYS: u32 = 30;

/// 启动事件流落盘后台任务（订阅 [`super::EventBus`]，JSONL 逐行追加）
///
/// 生命周期为 ServiceHandle 统一模式（`stop_tx` watch + join），同时接受
/// 应用关闭令牌兜底驱动（container 注入，任一先到即排空退出）；`stop_tx`
/// 被直接 drop（未经 stop）不视为停止指令，仅停用该信号源。
///
/// 订阅在 spawn 返回**前**同步建立：broadcast 只投递订阅之后发布的消息，
/// 先订阅再 spawn 可保证调用方拿到句柄前的 publish 也不丢。
pub fn spawn_event_sink(
    base_path: PathBuf,
    bus: Arc<super::EventBus>,
    shutdown_token: CancellationToken,
) -> crate::ServiceHandle {
    let rx = bus.subscribe();
    let (stop_tx, stop_rx) = watch::channel(false);
    let join_handle = tokio::spawn(async move {
        run_sink(base_path, rx, stop_rx, shutdown_token).await;
    });
    crate::ServiceHandle {
        stop_tx,
        join_handle,
        name: "event-sink",
    }
}

/// 清理超过保留天数的旧事件流文件，返回删除个数
///
/// 只清理 `YYYY-MM-DD.jsonl` 命名规范的文件；其他名字（外部放置、手工备份）
/// 一律不动——宁可少删，不可误删。供启动清理与 container 每日 housekeeping 共用。
pub fn cleanup_expired_event_files(events_dir: &Path, keep_days: u32) -> usize {
    // 与文件名同口径按 UTC 日期取 cutoff
    let cutoff = Utc::now().date_naive() - chrono::Duration::days(i64::from(keep_days));
    let Ok(entries) = std::fs::read_dir(events_dir) else {
        // 目录尚不存在 = 从未落盘，无事可清
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date_text) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff
            && let Err(e) = std::fs::remove_file(&path)
        {
            tracing::warn!("删除过期事件流文件失败 ({}): {e}", path.display());
            continue;
        }
        if date < cutoff {
            removed += 1;
        }
    }
    removed
}

/// 落盘主循环：收事件 → 写行 → 停止信号到达后排空积压 → flush 退出
async fn run_sink(
    base_path: PathBuf,
    mut rx: broadcast::Receiver<EventEnvelope>,
    mut stop_rx: watch::Receiver<bool>,
    shutdown_token: CancellationToken,
) {
    let events_dir = crate::utils::paths::events_dir(&base_path);
    // 启动即清理一次：housekeeping 的每日 tick 首个 tick 被跳过（与日志同因），
    // 跨日兜底由它接管
    cleanup_expired_event_files(&events_dir, EVENT_RETENTION_DAYS);
    let mut writer = SinkWriter::new(events_dir);
    // stop_tx 是否仍存活（句柄未经 stop 被 drop 后停用该信号源）
    let mut watch_alive = true;
    // 停止指令已到：不再阻塞等待新事件，仅排空积压后退出
    let mut stopping = false;
    loop {
        enum Step {
            Write(EventEnvelope),
            Done,
        }
        let step = if stopping {
            match rx.try_recv() {
                Ok(envelope) => Step::Write(envelope),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("事件流落盘停止排空时落后 {n} 条，最旧事件已丢弃（继续收后续）");
                    continue;
                }
                // Empty：积压已排完；Closed：总线已关闭。两者都到退出点
                Err(_) => Step::Done,
            }
        } else {
            tokio::select! {
                biased;
                // 关闭令牌（container 注入的应用级令牌 child）先到也算停止
                _ = shutdown_token.cancelled() => {
                    stopping = true;
                    continue;
                }
                changed = stop_rx.changed(), if watch_alive => {
                    match changed {
                        Ok(()) => {
                            if *stop_rx.borrow_and_update() {
                                stopping = true;
                            }
                            // Ok 但值仍为 false：假唤醒，回到循环继续等
                            continue;
                        }
                        Err(_) => {
                            // stop_tx 已 drop：仅停用该信号源，生命周期由关闭令牌兜底
                            watch_alive = false;
                            continue;
                        }
                    }
                }
                recv = rx.recv() => match recv {
                    Ok(envelope) => Step::Write(envelope),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Lagged 处理口径（见 bus 模块 doc）：warn 留痕继续收
                        tracing::warn!("事件流落盘落后 {n} 条，最旧事件已丢弃（继续收后续）");
                        continue;
                    }
                    // 总线关闭：所有发送端均已 drop（应用退出路径）
                    Err(broadcast::error::RecvError::Closed) => Step::Done,
                },
            }
        };
        match step {
            Step::Write(envelope) => writer.write_event(&envelope).await,
            Step::Done => break,
        }
    }
    // 停止时 flush 已缓冲行：BufWriter 里的尾巴不能丢
    writer.flush().await;
}

/// 事件行写入器：按 UTC 日期持有当前文件，跨日自动切换
struct SinkWriter {
    /// 事件流目录 `<base>/events`
    dir: PathBuf,
    /// 当前写入目标（文件按 UTC 日切分；`None` 表示尚未打开或上次写入失败待重开）
    current: Option<(NaiveDate, BufWriter<tokio::fs::File>)>,
}

impl SinkWriter {
    fn new(dir: PathBuf) -> Self {
        Self { dir, current: None }
    }

    /// 追加一行事件；任何失败仅 warn 并丢弃该条（见模块 doc 可靠性口径）
    async fn write_event(&mut self, envelope: &EventEnvelope) {
        // 序列化失败理论上不可达（纯内存结构、无自定义序列化），防御性跳过
        let Ok(mut line) = serde_json::to_string(envelope) else {
            tracing::warn!("事件序列化失败，跳过落盘 id={}", envelope.id);
            return;
        };
        line.push('\n');
        // 文件按事件 UTC 日期切分（见模块 doc 布局说明）
        let date = envelope.timestamp.date_naive();
        if let Some((open_date, _)) = &self.current
            && *open_date != date
        {
            // 跨日：flush 旧文件后切换
            self.flush().await;
            self.current = None;
        }
        if self.current.is_none()
            && let Err(e) = self.open(date).await
        {
            tracing::warn!("事件流文件打开失败 ({date})，本条事件丢弃: {e}");
            return;
        }
        let Some((_, file)) = self.current.as_mut() else {
            return;
        };
        if let Err(e) = file.write_all(line.as_bytes()).await {
            tracing::warn!("事件流写入失败，本条事件丢弃并待下次事件重开文件: {e}");
            // 置空以便下一条事件重开（半写入状态不可信）
            self.current = None;
        }
    }

    /// 打开（或创建）指定日期的追加写文件
    async fn open(&mut self, date: NaiveDate) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.dir.join(format!("{date}.jsonl"));
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        self.current = Some((date, BufWriter::new(file)));
        Ok(())
    }

    /// 将已缓冲行刷入底层文件；失败仅 warn（数据尽力而为，不阻断主循环）
    async fn flush(&mut self) {
        if let Some((_, file)) = &mut self.current
            && let Err(e) = file.flush().await
        {
            tracing::warn!("事件流 flush 失败: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AppEvent, EventBus, LoginSummary, NetworkSummary, TaskSummary};

    /// 停止后排空 + flush：stop 之前 publish 的事件全部落盘且行格式合法
    #[tokio::test]
    async fn test_sink_writes_jsonl_and_flushes_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::new());
        let sink = spawn_event_sink(
            dir.path().to_path_buf(),
            bus.clone(),
            CancellationToken::new(),
        );

        bus.publish(AppEvent::LoginSucceeded(LoginSummary {
            profile_id: "p-1".into(),
            channel: "browser".into(),
            reason: None,
            attempt: Some(1),
        }));
        bus.publish(AppEvent::TaskFailed(TaskSummary {
            task_id: "t-1".into(),
            task_kind: Some("script".into()),
            reason: Some("exit_code_1".into()),
        }));
        bus.publish(AppEvent::NetworkUp(NetworkSummary {
            profile_id: None,
            reason: None,
        }));

        // 停止触发排空 + flush，退出后文件内容即最终状态
        sink.stop_with_timeout(std::time::Duration::from_secs(2))
            .await;

        let file = dir
            .path()
            .join("events")
            .join(format!("{}.jsonl", Utc::now().date_naive()));
        let content = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "三条事件各一行: {content}");
        for (i, line) in lines.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("第 {i} 行不是合法 JSON: {e}: {line}"));
            // 每行必备字段：id（uuid）/ timestamp（RFC3339）/ type
            let id = value["id"].as_str().expect("应有 id");
            uuid::Uuid::parse_str(id).expect("id 应为合法 uuid");
            chrono::DateTime::parse_from_rfc3339(value["timestamp"].as_str().unwrap())
                .expect("timestamp 应为 RFC3339");
            assert!(value["type"].is_string(), "应有 type 判别字段");
        }
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "login_succeeded");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["type"], "task_failed");
        assert_eq!(second["task_id"], "t-1");
    }

    /// 无事件时不建文件（惰性打开）
    #[tokio::test]
    async fn test_sink_no_events_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::new());
        let sink = spawn_event_sink(
            dir.path().to_path_buf(),
            bus.clone(),
            CancellationToken::new(),
        );
        sink.stop_with_timeout(std::time::Duration::from_secs(2))
            .await;
        assert!(
            !dir.path().join("events").exists()
                || std::fs::read_dir(dir.path().join("events"))
                    .unwrap()
                    .count()
                    == 0,
            "未落盘前不应创建事件文件"
        );
    }

    /// 关闭令牌驱动：令牌取消后 sink 排空退出（无需显式 stop）
    #[tokio::test]
    async fn test_sink_stops_on_shutdown_token() {
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::new());
        let token = CancellationToken::new();
        let sink = spawn_event_sink(dir.path().to_path_buf(), bus.clone(), token.clone());
        bus.publish(AppEvent::LoginSucceeded(LoginSummary {
            profile_id: "p-2".into(),
            channel: "http".into(),
            reason: None,
            attempt: None,
        }));
        token.cancel();
        // stop_with_timeout 应在令牌触发后退出口（而非等待超时中止）
        sink.stop_with_timeout(std::time::Duration::from_secs(2))
            .await;
        let file = dir
            .path()
            .join("events")
            .join(format!("{}.jsonl", Utc::now().date_naive()));
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
    }

    /// 轮转清理：过期日期文件被删，近期与命名不规范的文件保留
    #[test]
    fn test_cleanup_expired_event_files() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events");
        std::fs::create_dir_all(&events).unwrap();
        let today = Utc::now().date_naive();
        let write_file = |name: &str| {
            std::fs::write(events.join(name), b"{}\n").unwrap();
        };
        write_file(&format!("{}.jsonl", (today - chrono::Duration::days(31))));
        write_file(&format!("{}.jsonl", (today - chrono::Duration::days(29))));
        write_file(&format!("{today}.jsonl"));
        write_file("not-a-date.jsonl");
        write_file("2020-01-01.txt");

        let removed = cleanup_expired_event_files(&events, 30);
        assert_eq!(removed, 1, "仅 31 天前的文件过期");
        assert!(
            !events
                .join(format!("{}.jsonl", (today - chrono::Duration::days(31))))
                .exists()
        );
        assert!(
            events
                .join(format!("{}.jsonl", (today - chrono::Duration::days(29))))
                .exists()
        );
        assert!(events.join(format!("{today}.jsonl")).exists());
        assert!(
            events.join("not-a-date.jsonl").exists(),
            "命名不规范的文件不动"
        );
        assert!(events.join("2020-01-01.txt").exists(), "非 .jsonl 文件不动");
    }

    /// 目录不存在时清理为空操作（首次启动、从未落盘）
    #[test]
    fn test_cleanup_on_missing_dir_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            cleanup_expired_event_files(&dir.path().join("events"), 30),
            0
        );
    }
}
