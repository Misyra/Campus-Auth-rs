//! 调度主循环模块。
//!
//! 负责 cron 5→7 字段解析、`SystemTime`↔`Instant` 单调时钟转换、主循环
//! `tokio::select!` 调度、到期任务触发分发，以及时钟边界（休眠唤醒 / 重启 /
//! NTP 回拨 / DST 切换）的处理。

use std::sync::Arc;
use std::time::{Duration as StdDuration, SystemTime};

use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep_until, Duration as TokioDuration, Instant as TokioInstant};

use crate::config::runtime::ConfigReloadSignal;
use crate::scheduler::task::{
    CRON_PARSE_PREFIX, CRON_PARSE_SUFFIX, DEFAULT_SCHEDULED_TIMEOUT, ScheduledTask,
};
use crate::scheduler::{SchedulerError, SchedulerService, TaskChange};

/// 调度循环的睡眠动作。
pub(crate) enum SleepAction {
    /// 立即触发（已到期 / 极近未来 / 时钟回拨导致的过去时间）。
    FireNow,
    /// 睡眠到指定单调时刻。
    SleepUntil {
        instant: TokioInstant,
        #[allow(dead_code)]
        duration: TokioDuration,
    },
}

/// 单个任务的运行时调度推导（不持久化）。
pub(crate) struct TaskSchedule {
    pub(crate) task_id: String,
    pub(crate) schedule: Option<Schedule>,
    pub(crate) next_fire_at: Option<SystemTime>,
}

/// 将用户输入的 5 字段 cron 转换为 cron crate 所需的 7 字段表达式并解析。
///
/// 转换规则：`{prefix}{five_field}{suffix}` = `"0 " + 原5字段 + " *"`，
/// 例如 `"0 8 * * *"` → `"0 0 8 * * * *"`。
pub(crate) fn parse_cron_expr(five_field: &str) -> Result<Schedule, SchedulerError> {
    let trimmed = five_field.trim();
    let field_count = trimmed.split_whitespace().count();
    if field_count != 5 {
        return Err(SchedulerError::InvalidCronExpr(
            five_field.to_string(),
            format!("期望 5 个字段，实际 {} 个", field_count),
        ));
    }
    let seven_field = parse_cron_5_to_7(trimmed);
    seven_field
        .parse::<Schedule>()
        .map_err(|e| SchedulerError::InvalidCronExpr(five_field.to_string(), e.to_string()))
}

/// 拼接 5→7 字段字符串（导出供测试复用）。
pub(crate) fn parse_cron_5_to_7(five_field_trimmed: &str) -> String {
    format!("{}{}{}", CRON_PARSE_PREFIX, five_field_trimmed, CRON_PARSE_SUFFIX)
}

/// 将目标墙钟时间转换为调度睡眠动作。
///
/// 每次迭代都重新基于当前 `SystemTime`/`Instant` 计算，不跨迭代缓存
/// （`Instant` 单调、`SystemTime` 可因 NTP 回拨，缓存会失效）。
pub(crate) fn systemtime_to_sleep_target(target: SystemTime) -> SleepAction {
    let now_sys = SystemTime::now();
    match target.duration_since(now_sys) {
        Ok(dur) if dur.is_zero() || dur < StdDuration::from_millis(100) => SleepAction::FireNow,
        Ok(dur) => SleepAction::SleepUntil {
            instant: TokioInstant::now() + dur,
            duration: dur,
        },
        Err(_) => SleepAction::FireNow,
    }
}

/// 计算全局最近的触发时间。
pub(crate) fn compute_nearest_fire_at(schedules: &[TaskSchedule]) -> Option<SystemTime> {
    schedules.iter().filter_map(|ts| ts.next_fire_at).min()
}

/// 扫描磁盘、解析 cron、重建内存缓存与调度表。
///
/// 解析失败或禁用的任务会被跳过（不中断整体加载）。
pub(crate) fn load_and_parse_all(service: &SchedulerService) -> Vec<TaskSchedule> {
    let dir = &service.scheduled_dir;
    let mut loaded: Vec<ScheduledTask> = Vec::new();
    let mut schedules: Vec<TaskSchedule> = Vec::new();
    let mut parse_failures: u32 = 0;

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = match path.file_name().and_then(|f| f.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if fname.starts_with('.') || !fname.ends_with(".json") {
                continue;
            }
            let id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            let task = match ScheduledTask::load_from(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("加载定时任务 {} 失败: {}", id, e);
                    continue;
                }
            };
            if !task.enabled {
                continue;
            }

            let schedule = match parse_cron_expr(&task.cron) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("定时任务 {} cron 解析失败: {}", id, e);
                    parse_failures += 1;
                    None
                }
            };
            let next_fire_at = schedule
                .as_ref()
                .and_then(|s| s.upcoming(Local).next())
                .map(systemtime_from_local);

            loaded.push(task);
            schedules.push(TaskSchedule {
                task_id: id,
                schedule,
                next_fire_at,
            });
        }
    }

    if parse_failures > 0 {
        tracing::warn!(
            "定时任务加载完成: 共加载 {} 个任务，{} 个 cron 解析失败",
            loaded.len(),
            parse_failures
        );
    }

    service.update_state(|s| {
        s.tasks = loaded;
    });
    schedules
}

/// [`load_and_parse_all`] 的异步封装。
///
/// 磁盘扫描与 cron 解析属同步阻塞 I/O，放入 `spawn_blocking` 执行，
/// 避免在调度主循环的 tokio worker 线程上同步读盘阻塞 `select!`。
async fn load_and_parse_all_async(service: &Arc<SchedulerService>) -> Vec<TaskSchedule> {
    let svc = service.clone();
    match tokio::task::spawn_blocking(move || load_and_parse_all(&svc)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("加载定时任务的阻塞任务失败: {e}");
            Vec::new()
        }
    }
}

/// `DateTime<Local>` → `SystemTime`（经 UTC 中转，避免依赖不确定的 `From` 实现）。
fn systemtime_from_local(dt: DateTime<Local>) -> SystemTime {
    let utc: DateTime<Utc> = dt.into();
    utc.into()
}

/// 触发所有到期任务（每个任务最多执行一次），并重新计算其下次触发时间。
///
/// 不循环补偿：即使 `now` 超过 `next_fire_at` 多个周期，也只触发一次。
pub(crate) fn fire_due_tasks(
    service: Arc<SchedulerService>,
    schedules: &mut [TaskSchedule],
    now: SystemTime,
) -> usize {
    let mut fired = 0;
    for ts in schedules.iter_mut() {
        let fire_at = match ts.next_fire_at {
            Some(t) => t,
            None => continue,
        };
        if fire_at > now {
            continue;
        }

        if let Some(task) = service.get_task(&ts.task_id) {
            if !task.enabled {
                // 任务在两次 reload 之间被禁用，跳过执行但更新下次触发时间
                ts.next_fire_at = ts
                    .schedule
                    .as_ref()
                    .and_then(|s| s.upcoming(Local).next())
                    .map(systemtime_from_local);
                continue;
            }
            let svc = service.clone();
            let sem = service.concurrency.clone();
            tokio::spawn(async move {
                // 获取并发许可，限制同时执行的到期任务数，避免无上限 spawn（历史遗留 F10）
                if let Ok(_permit) = sem.acquire_owned().await {
                    execute_scheduled_task(task, svc).await;
                }
            });
        }

        ts.next_fire_at = ts
            .schedule
            .as_ref()
            .and_then(|s| s.upcoming(Local).next())
            .map(systemtime_from_local);
        fired += 1;
    }
    fired
}

/// 在独立 tokio task 中执行到期任务（不阻塞主循环）。
/// 此函数同时供定时触发与手动触发使用。
pub async fn execute_scheduled_task(task: ScheduledTask, service: Arc<SchedulerService>) {
    let start = TokioInstant::now();
    let task_id = task.id.clone();
    let target_id = task.target_id.clone();

    // 任务类型由 target_id 关联的目标任务权威推导（TaskKind），不再冗余存储 task_type。
    let (success, message) = match service.task_manager.load_task(&target_id).await {
        Ok(crate::tasks::TaskKind::Browser(cfg)) => {
            if task.profile_id.is_some() {
                // 登录语义：带凭据 → LoginOrchestrator.submit（重试 + 网络验证）
                let timeout = task.timeout.unwrap_or(DEFAULT_SCHEDULED_TIMEOUT);
                let handle = service
                    .orchestrator
                    .submit(
                        crate::status::LoginSource::Browser,
                        Some(target_id.clone()),
                        task.profile_id.clone(),
                    )
                    .await;
                // 与 Script/Shell 分支一致，为等待结果加超时上限，避免登录流程卡住时无限等待（历史遗留 F9）
                // 超时预算归属：调度器为外层 deadline 所有者。超时时主动 cancel 句柄，让登录
                // 状态机退出并释放 Worker（cancel 会经 cancel_token → CancelRegistry → Worker 传递），
                // 而非仅丢弃 await_result future、任登录在后台继续跑。
                match tokio::time::timeout(
                    TokioDuration::from_secs(timeout),
                    handle.await_result(),
                )
                .await
                {
                    Ok(result) => (result.success, result.message),
                    Err(_) => {
                        handle.cancel();
                        (false, format!("执行超时: {}s", timeout))
                    }
                }
            } else {
                // 通用语义：打卡/签到 → execute_browser（不注入凭据，步骤完成即成功）
                let timeout = task.timeout.unwrap_or(DEFAULT_SCHEDULED_TIMEOUT);
                match tokio::time::timeout(
                    TokioDuration::from_secs(timeout),
                    service.executor.execute_browser(&cfg),
                )
                .await
                {
                    Ok(Ok(r)) => (r.success, r.output),
                    Ok(Err(e)) => (false, format!("执行错误: {}", e)),
                    Err(_) => (false, format!("执行超时: {}s", timeout)),
                }
            }
        }

        Ok(crate::tasks::TaskKind::Script(cfg)) => {
            let timeout = task.timeout.unwrap_or(DEFAULT_SCHEDULED_TIMEOUT);
            match tokio::time::timeout(
                TokioDuration::from_secs(timeout),
                service.executor.execute_script(&cfg),
            )
            .await
            {
                Ok(Ok(r)) => (r.success, format!("exit={}, {}", r.exit_code, r.output)),
                Ok(Err(e)) => (false, format!("执行错误: {}", e)),
                Err(_) => (false, format!("执行超时: {}s", timeout)),
            }
        }

        Ok(crate::tasks::TaskKind::Shell(cfg)) => {
            let timeout = task.timeout.unwrap_or(DEFAULT_SCHEDULED_TIMEOUT);
            match tokio::time::timeout(
                TokioDuration::from_secs(timeout),
                service.executor.execute_shell(&cfg),
            )
            .await
            {
                Ok(Ok(r)) => (r.success, format!("exit={}, {}", r.exit_code, r.output)),
                Ok(Err(e)) => (false, format!("执行错误: {}", e)),
                Err(_) => (false, format!("执行超时: {}s", timeout)),
            }
        }

        Err(e) => {
            let msg = format!("加载目标任务失败: {target_id} ({e})");
            tracing::warn!("{}", msg);
            (false, msg)
        }
    };

    let duration = start.elapsed();
    let status_str = if success { "success" } else { "failure" };

    service.update_last_run(&task_id, status_str, &message);
    service.add_history_record(&task_id, status_str, &message, duration);

    if success {
        tracing::info!(
            "定时任务 {} 执行成功 ({:.1}s)",
            task_id,
            duration.as_secs_f64()
        );
    } else {
        // 安全截断：按 Unicode 字符边界截取，避免 UTF-8 字节索引 panic
        let preview: String = message.chars().take(100).collect();
        tracing::warn!(
            "定时任务 {} 执行失败: {} ({:.1}s)",
            task_id,
            preview,
            duration.as_secs_f64()
        );
    }
}

/// 调度主循环：单 tokio task，通过 `tokio::select!`（`biased`）同时等待
/// 停止信号、任务变更、`ConfigReloadSignal` 与 sleep 到期。
pub(crate) async fn cron_loop(
    service: Arc<SchedulerService>,
    mut stop_rx: watch::Receiver<bool>,
    task_change_rx: Option<mpsc::Receiver<TaskChange>>,
    reload_rx: Option<mpsc::Receiver<ConfigReloadSignal>>,
) {
    let mut task_change_rx_opt = task_change_rx;
    let mut reload_rx_opt = reload_rx;

    let mut task_schedules = load_and_parse_all_async(&service).await;

    loop {
        let nearest = compute_nearest_fire_at(&task_schedules);
        service.update_state(|s| {
            s.next_fire_at = nearest;
            s.running = true;
        });
        service.publish_status();

        let sleep_action = match nearest {
            Some(t) => systemtime_to_sleep_target(t),
            None => SleepAction::SleepUntil {
                instant: TokioInstant::now() + TokioDuration::from_secs(86400),
                duration: TokioDuration::from_secs(86400),
            },
        };

        tokio::select! {
            biased;

            // 停止信号（最高优先级）
            res = stop_rx.changed() => {
                if res.is_err() || *stop_rx.borrow() {
                    tracing::info!("调度器收到停止信号，退出主循环");
                    break;
                }
            }

            // 内部任务配置变更通知（CRUD 触发）
            change = async {
                match &mut task_change_rx_opt {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match change {
                    Some(TaskChange::Reload) => {
                        // 重载前先触发已到期任务，避免 reload 落在“到期~触发”窄窗口内造成静默漏触发（历史遗留 F5）
                        let fired = fire_due_tasks(service.clone(), &mut task_schedules, SystemTime::now());
                        if fired > 0 {
                            tracing::info!("重载前触发 {} 个到期任务", fired);
                        }
                        task_schedules = load_and_parse_all_async(&service).await;
                        tracing::debug!("调度器重载任务列表，共 {} 个任务", task_schedules.len());
                    }
                    None => {
                        tracing::warn!("定时任务变更 channel 已关闭，停止监听变更");
                        task_change_rx_opt = None;
                    }
                }
            }

            // 配置重载信号（ConfigService 触发）
            sig = async {
                match &mut reload_rx_opt {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match sig {
                    Some(ConfigReloadSignal::TasksChanged)
                    | Some(ConfigReloadSignal::GlobalChanged) => {
                        // 重载前先触发已到期任务，避免窄窗口漏触发（历史遗留 F5）
                        let fired = fire_due_tasks(service.clone(), &mut task_schedules, SystemTime::now());
                        if fired > 0 {
                            tracing::info!("重载前触发 {} 个到期任务", fired);
                        }
                        task_schedules = load_and_parse_all_async(&service).await;
                        tracing::debug!(
                            "配置变更触发调度器重载，共 {} 个任务",
                            task_schedules.len()
                        );
                    }
                    Some(ConfigReloadSignal::ProfileSwitched { .. }) => {
                        // 仅切换 Profile，不影响定时任务表
                    }
                    None => {
                        tracing::debug!("配置重载 channel 已关闭，停止监听配置变更");
                        reload_rx_opt = None;
                    }
                }
            }

            // sleep 到期
            _ = async {
                match sleep_action {
                    SleepAction::FireNow => {}
                    SleepAction::SleepUntil { instant, .. } => {
                        sleep_until(instant).await;
                    }
                }
            } => {
                let now = SystemTime::now();
                let fired = fire_due_tasks(service.clone(), &mut task_schedules, now);
                if fired > 0 {
                    tracing::info!("调度周期: 触发 {} 个到期任务", fired);
                }
            }
        }
    }

    service.update_state(|s| s.running = false);
    service.publish_status();
    tracing::info!("调度器主循环已退出");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn test_parse_cron_5_to_7() {
        let s = parse_cron_expr("0 8 * * *").unwrap();
        let seven = parse_cron_5_to_7("0 8 * * *");
        assert_eq!(seven, "0 0 8 * * * *");
        let next = s.upcoming(Local).next().unwrap();
        assert_eq!(next.hour(), 8);
    }

    #[test]
    fn test_parse_cron_invalid() {
        assert!(parse_cron_expr("abc").is_err());
        assert!(parse_cron_expr("0 0 8 * * *").is_err()); // 6 字段应拒绝
    }

    #[test]
    fn test_sleep_target_past_is_fire_now() {
        let past = SystemTime::now() - StdDuration::from_secs(10);
        assert!(matches!(systemtime_to_sleep_target(past), SleepAction::FireNow));
    }

    #[test]
    fn test_sleep_target_future_is_sleep_until() {
        let future = SystemTime::now() + StdDuration::from_secs(100);
        match systemtime_to_sleep_target(future) {
            SleepAction::SleepUntil { duration, .. } => {
                // 允许微秒级浮点精度误差
                let diff = duration.as_secs_f64() - 100.0;
                assert!(diff.abs() < 0.1, "duration 偏差过大: {diff}");
            }
            _ => panic!("应为 SleepUntil"),
        }
    }

    #[test]
    fn test_compute_nearest() {
        let t1 = SystemTime::now() + StdDuration::from_secs(100);
        let t2 = SystemTime::now() + StdDuration::from_secs(50);
        let schedules = vec![
            TaskSchedule { task_id: "a".into(), schedule: None, next_fire_at: Some(t1) },
            TaskSchedule { task_id: "b".into(), schedule: None, next_fire_at: Some(t2) },
        ];
        assert_eq!(compute_nearest_fire_at(&schedules), Some(t2));
    }
}
