//! 调度主循环模块。
//!
//! 负责 cron 5→7 字段解析、目标墙钟时刻的分片睡眠（≤60s/片，每片按
//! `SystemTime` 重估剩余时长，抵御休眠唤醒/墙钟前跳导致的过睡）、主循环
//! `tokio::select!` 调度、到期任务触发分发、外部删除的低频兜底扫描，
//! 以及时钟边界（休眠唤醒 / 重启 / NTP 回拨 / DST 切换）的处理。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration as StdDuration, SystemTime};

use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration as TokioDuration, Instant as TokioInstant, sleep};

use crate::config::runtime::ConfigReloadSignal;
use crate::scheduler::task::{
    CRON_PARSE_PREFIX, CRON_PARSE_SUFFIX, DEFAULT_SCHEDULED_TIMEOUT, ScheduledTask,
};
use crate::scheduler::{SchedulerError, SchedulerService, TaskChange};

/// 单片睡眠上限：长睡眠切成 ≤60s 的短片，每片醒来按墙钟重估剩余时长，
/// 抵御 Linux 休眠唤醒（CLOCK_MONOTONIC 不计入挂起时间）与墙钟前跳导致的过睡
const MAX_SLEEP_SLICE: TokioDuration = TokioDuration::from_secs(60);

/// 外部删除兜底扫描周期：运行期外部直接删除 tasks/scheduled/*.json 不触发任何
/// 内部变更通知，靠低频扫描发现并从调度表移除
const EXTERNAL_DELETE_SCAN_INTERVAL: TokioDuration = TokioDuration::from_secs(300);

/// 调度循环的睡眠动作。
pub(crate) enum SleepAction {
    /// 立即触发（已到期 / 极近未来 / 时钟回拨导致的过去时间）。
    FireNow,
    /// 睡眠到指定墙钟时刻（分片执行，每片醒来重估剩余时长）。
    SleepUntil {
        /// 目标墙钟时刻
        target: SystemTime,
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
    format!(
        "{}{}{}",
        CRON_PARSE_PREFIX, five_field_trimmed, CRON_PARSE_SUFFIX
    )
}

/// 将目标墙钟时间转换为调度睡眠动作。
///
/// 每次迭代都重新基于当前 `SystemTime`/`Instant` 计算，不跨迭代缓存
/// （`Instant` 单调、`SystemTime` 可因 NTP 回拨，缓存会失效）。
pub(crate) fn systemtime_to_sleep_target(target: SystemTime) -> SleepAction {
    let now_sys = SystemTime::now();
    match target.duration_since(now_sys) {
        // 距目标不足 100ms 视为到期，直接触发（避免极短睡眠的调度抖动）
        Ok(dur) if dur < StdDuration::from_millis(100) => SleepAction::FireNow,
        Ok(_) => SleepAction::SleepUntil { target },
        // 目标在过去（墙钟回拨越过目标）：立即触发，由 fire_due_tasks 的
        // 单次补偿去重逻辑决定是否触发并前移下次触发点
        Err(_) => SleepAction::FireNow,
    }
}

/// 分片睡眠直到墙钟目标到期（F12）。
///
/// `Instant` 在 Linux 休眠唤醒 / 墙钟前跳时可能显著长于真实墙钟差
/// （CLOCK_MONOTONIC 不计入挂起时间），一次性换算的单调目标会导致过睡。
/// 此处将长睡眠切成 ≤60s 的短片，每片醒来后用 `SystemTime::now()` 重估剩余
/// 目标（目标本身来自 `schedule.upcoming().next()` 的墙钟推导）：到期则返回，
/// 未到期则按新的墙钟差重新换算单调时长继续睡；墙钟回拨越过目标时
/// `duration_since` 返回 Err，同样返回并交给 `fire_due_tasks` 判定。
async fn sleep_until_wallclock(target: SystemTime) {
    loop {
        let Ok(remaining) = target.duration_since(SystemTime::now()) else {
            return;
        };
        if remaining.is_zero() {
            return;
        }
        sleep(remaining.min(MAX_SLEEP_SLICE)).await;
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
    // cron 解析失败或日历上永无匹配的 enabled 任务 ID：写入状态供 API 查询（M7 失效可见性）
    let mut invalid_ids: HashSet<String> = HashSet::new();

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
            // 所有任务（含禁用）都进缓存：get_task/toggle_task/update_task/run_task
            // 均基于内存缓存，跳过禁用任务会让它们 404、永远无法被重新启用
            if task.enabled {
                let schedule = match parse_cron_expr(&task.cron) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!("定时任务 {} cron 解析失败: {}", id, e);
                        parse_failures += 1;
                        invalid_ids.insert(id.clone());
                        None
                    }
                };
                // 解析成功还需检查日历可达性：如 "0 0 31 4 *"（4 月没有 31 日）
                // 语法合法但日历上永无匹配，任务同样静默永不触发，必须一并纳入
                // invalid_cron_ids 供前端展示"表达式无效"（G9，M7 失效可见性）
                let next_fire_at = match schedule.as_ref().and_then(|s| s.upcoming(Local).next()) {
                    Some(next) => Some(systemtime_from_local(next)),
                    None => {
                        if schedule.is_some() {
                            tracing::warn!(
                                "定时任务 {} cron 日历上永无匹配（如 31 日落在不存在的月份）: {}",
                                id,
                                task.cron
                            );
                            invalid_ids.insert(id.clone());
                        }
                        None
                    }
                };
                schedules.push(TaskSchedule {
                    task_id: id,
                    schedule,
                    next_fire_at,
                });
            }
            loaded.push(task);
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
        s.invalid_cron_ids = invalid_ids;
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

/// 扫描磁盘上的定时任务文件 id 集合（同步阻塞 I/O，供 spawn_blocking 使用）。
fn scan_disk_task_ids(dir: &std::path::Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return ids;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            ids.insert(stem.to_string());
        }
    }
    ids
}

/// 外部删除兜底扫描（F12b）：对比磁盘任务文件 id 集合与内存调度表，
/// 磁盘上已消失的 id 从调度表与内存缓存移除并记日志。
///
/// **只做删除检测，不对账外部修改**：任务修改语义上只认 TaskManager/调度器的
/// 保存通道（内部 CRUD → `TaskChange::Reload` 全量重载），外部直接改文件的内容
/// 不被采纳——内存中可能存在用户正在进行的编辑态（前端编辑 → 保存的窗口期），
/// 若按磁盘内容对账会静默覆盖未保存的编辑。删除则无此歧义（文件消失即意图明确），
/// 低频（5 分钟）扫描 + 立即从调度表移除即可避免已删除任务继续被触发。
async fn reconcile_external_deletions(
    service: &Arc<SchedulerService>,
    schedules: &mut Vec<TaskSchedule>,
) {
    let dir = service.scheduled_dir.clone();
    let disk_ids = match tokio::task::spawn_blocking(move || scan_disk_task_ids(&dir)).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("外部删除扫描的阻塞任务失败: {e}");
            return;
        }
    };

    let before = schedules.len();
    schedules.retain(|ts| {
        let exists = disk_ids.contains(&ts.task_id);
        if !exists {
            tracing::info!(
                task_id = %ts.task_id,
                "定时任务文件已被外部删除，从调度表移除"
            );
        }
        exists
    });
    if schedules.len() != before {
        // 同步清理内存缓存（get_task/toggle 等基于该缓存，避免对外部已删除的任务继续可见）
        service.update_state(|s| {
            s.tasks.retain(|t| disk_ids.contains(&t.id));
        });
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
            // 同一任务上一轮尚未结束：跳过本轮触发，防止执行时间长于
            // cron 周期的任务重叠运行（如重复操作同一浏览器实例）
            if !service.try_mark_running(&ts.task_id) {
                tracing::warn!(
                    task_id = %ts.task_id,
                    "上一轮执行仍在进行，跳过本轮定时触发"
                );
                ts.next_fire_at = ts
                    .schedule
                    .as_ref()
                    .and_then(|s| s.upcoming(Local).next())
                    .map(systemtime_from_local);
                continue;
            }
            service.clone().spawn_tracked_run(task);
        } else {
            // 到期瞬间任务刚被删除（内存缓存已无）：仅 debug 留痕，下一轮调度表重载后消失
            tracing::debug!(task_id = %ts.task_id, "到期任务已不存在于内存缓存，跳过触发");
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

/// "执行中"标记的 RAII 守卫：drop 时清除标记，覆盖正常结束/超时/异常所有路径
struct RunningGuard {
    service: Arc<SchedulerService>,
    task_id: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.service.clear_running(&self.task_id);
    }
}

/// 组装定时任务执行结果消息：脚本/Shell 任务附带退出码，浏览器任务仅有输出文本
fn scheduled_result_message(task: &crate::tasks::TaskKind, r: &crate::tasks::TaskResult) -> String {
    match task {
        crate::tasks::TaskKind::Browser(_) => r.output.clone(),
        _ => format!("exit={}, {}", r.exit_code, r.output),
    }
}

/// 在独立 tokio task 中执行到期任务（不阻塞主循环）。
/// 此函数同时供定时触发与手动触发使用。
pub async fn execute_scheduled_task(task: ScheduledTask, service: Arc<SchedulerService>) {
    let start = TokioInstant::now();
    let task_id = task.id.clone();
    let target_id = task.target_id.clone();
    // 执行结束（含异常）时清除"执行中"标记，恢复该任务的下一轮触发资格
    let _running_guard = RunningGuard {
        service: service.clone(),
        task_id: task_id.clone(),
    };

    // 任务类型由 target_id 关联的目标任务权威推导（TaskKind），不再冗余存储 task_type。
    let (success, message) = match service.task_manager.load_task(&target_id).await {
        Ok(kind) => {
            // 定时浏览器任务统一走通用语义（打卡/签到等日常自动化），不注入账号密码。
            // 登录认证由断网自动触发（LoginSource::Auto）或手动登录按钮负责，二者正交。
            //
            // 超时覆写（默认值 + 浏览器毫秒 / 脚本与 Shell 秒的单位差异 + 钳制）
            // 由 executor 的统一入口内部集中处理，此处只传秒数。
            // 注意绑定名用 kind：外层 `task: ScheduledTask` 持有 timeout 定义
            let timeout = task.timeout.unwrap_or(DEFAULT_SCHEDULED_TIMEOUT);
            match service
                .executor
                .execute_with_timeout_override(&kind, timeout)
                .await
            {
                Ok(r) => (r.success, scheduled_result_message(&kind, &r)),
                Err(e) => (false, format!("执行错误: {}", e)),
            }
        }

        Err(e) => {
            let msg = format!("加载目标任务失败: {target_id} ({e})");
            tracing::warn!(task_id = %task_id, target_id = %target_id, error = %e, "加载目标任务失败");
            (false, msg)
        }
    };

    let duration = start.elapsed();
    let status_str = if success { "success" } else { "failure" };

    service
        .update_last_run(&task_id, status_str, &message)
        .await;
    service
        .add_history_record(&task_id, status_str, &message, duration)
        .await;

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

    // 任务通知（接线原死开关 app.task_notification）：与登录失败通知同机制，
    // 经 notification 日志源推送到前端日志流，由用户在设置页开关
    if service.config.runtime().load().app.task_notification {
        // 安全截断：按 Unicode 字符边界截取，避免 UTF-8 字节索引 panic
        let preview: String = message.chars().take(120).collect();
        let notify = format!(
            "定时任务「{}」{} ({:.1}s）：{}",
            task.name,
            if success {
                "执行成功"
            } else {
                "执行失败"
            },
            duration.as_secs_f64(),
            preview
        );
        if success {
            tracing::info!(target: "notification", "{notify}");
        } else {
            tracing::warn!(target: "notification", "{notify}");
        }
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

    // task_change channel 关闭后的降级轮询定时器（条件守护，channel 正常时不生效）。
    // 首个 tick 立即就绪，先消费掉避免进入降级模式时连发重载（MissedTick::Skip 兜底）
    let mut degrade_poll = tokio::time::interval(TokioDuration::from_secs(60));
    degrade_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    degrade_poll.tick().await;

    // 外部删除兜底扫描定时器（5 分钟一次，见 reconcile_external_deletions）。
    // 首个 tick 立即就绪，先消费掉避免启动时立即做一次无意义扫描
    let mut external_delete_scan = tokio::time::interval(EXTERNAL_DELETE_SCAN_INTERVAL);
    external_delete_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    external_delete_scan.tick().await;

    loop {
        let nearest = compute_nearest_fire_at(&task_schedules);
        service.update_state(|s| {
            s.next_fire_at = nearest;
            s.running = true;
        });
        service.publish_status();

        let sleep_action = match nearest {
            Some(t) => systemtime_to_sleep_target(t),
            // 无任务时睡一个"很长"的空闲周期：同样走分片睡眠，墙钟异常时
            // 也会被外部删除扫描（5 分钟）周期性唤醒重估
            None => SleepAction::SleepUntil {
                target: SystemTime::now() + StdDuration::from_secs(86400),
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
                    Some(ConfigReloadSignal::GlobalChanged) => {
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
                        tracing::warn!("配置重载 channel 已关闭，停止监听配置变更");
                        reload_rx_opt = None;
                    }
                }
            }

            // 降级轮询：task_change channel 关闭后（发送端异常 drop），
            // CRUD 变更将无法唤醒 select，最长 86400s 内不被感知。
            // 每 60s 重载一次任务列表兜底（M7），channel 正常时此分支被条件禁用
            _ = degrade_poll.tick(), if task_change_rx_opt.is_none() => {
                let fired = fire_due_tasks(service.clone(), &mut task_schedules, SystemTime::now());
                if fired > 0 {
                    tracing::info!("降级轮询触发 {} 个到期任务", fired);
                }
                task_schedules = load_and_parse_all_async(&service).await;
                tracing::debug!("降级轮询重载任务列表，共 {} 个任务", task_schedules.len());
            }

            // 外部删除兜底扫描（F12b）：外部直接删除 tasks/scheduled/*.json 不触发
            // 内部变更通知，低频对比磁盘与内存调度表，移除磁盘上已消失的任务
            _ = external_delete_scan.tick() => {
                reconcile_external_deletions(&service, &mut task_schedules).await;
            }

            // sleep 到期（分片睡眠，每片醒来按墙钟重估，见 sleep_until_wallclock）
            _ = async {
                match sleep_action {
                    SleepAction::FireNow => {}
                    SleepAction::SleepUntil { target } => sleep_until_wallclock(target).await,
                }
            } => {
                let now = SystemTime::now();
                let fired = fire_due_tasks(service.clone(), &mut task_schedules, now);
                if fired > 0 {
                    tracing::info!(count = fired, "调度周期：触发到期任务");
                }
            }
        }
    }

    service.shutdown_tracked_runs().await;
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
    fn test_never_matching_cron_has_no_upcoming() {
        // G9："0 0 31 4 *"（4 月 31 日不存在）语法解析成功，
        // 但日历上永无匹配 → upcoming().next() 为 None，应被纳入 invalid_cron_ids
        let s = parse_cron_expr("0 0 31 4 *").expect("语法应解析成功");
        assert!(
            s.upcoming(Local).next().is_none(),
            "日历不可达的 cron 不应有下次触发点"
        );
        // 对照组：普通表达式一定有下次触发点
        let normal = parse_cron_expr("0 8 * * *").unwrap();
        assert!(normal.upcoming(Local).next().is_some());
    }

    #[test]
    fn test_sleep_target_past_is_fire_now() {
        let past = SystemTime::now() - StdDuration::from_secs(10);
        assert!(matches!(
            systemtime_to_sleep_target(past),
            SleepAction::FireNow
        ));
    }

    #[test]
    fn test_sleep_target_near_future_is_fire_now() {
        // 距目标不足 100ms 视为到期立即触发
        let near = SystemTime::now() + StdDuration::from_millis(50);
        assert!(matches!(
            systemtime_to_sleep_target(near),
            SleepAction::FireNow
        ));
    }

    #[test]
    fn test_sleep_target_future_keeps_wallclock_target() {
        // 远期目标转为 SleepUntil 并保留墙钟时刻（供分片睡眠逐片重估）
        let future = SystemTime::now() + StdDuration::from_secs(100);
        match systemtime_to_sleep_target(future) {
            SleepAction::SleepUntil { target } => {
                let diff = target
                    .duration_since(future)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                assert!(diff.abs() < 0.1, "target 应等于输入墙钟时刻");
            }
            _ => panic!("应为 SleepUntil"),
        }
    }

    #[test]
    fn test_compute_nearest() {
        let t1 = SystemTime::now() + StdDuration::from_secs(100);
        let t2 = SystemTime::now() + StdDuration::from_secs(50);
        let schedules = vec![
            TaskSchedule {
                task_id: "a".into(),
                schedule: None,
                next_fire_at: Some(t1),
            },
            TaskSchedule {
                task_id: "b".into(),
                schedule: None,
                next_fire_at: Some(t2),
            },
        ];
        assert_eq!(compute_nearest_fire_at(&schedules), Some(t2));
    }

    #[test]
    fn test_scan_disk_task_ids() {
        // 只收集非隐藏 .json 文件的 stem（与 load_and_parse_all 的文件筛选一致）
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("b.json"), "{}").unwrap();
        std::fs::write(tmp.path().join(".hidden.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("c.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("sub.json")).unwrap();

        let ids = scan_disk_task_ids(tmp.path());
        assert!(ids.contains("a"));
        assert!(ids.contains("b"));
        assert_eq!(ids.len(), 2);
    }

    #[tokio::test]
    async fn test_sleep_until_wallclock_returns_immediately_when_due() {
        // 目标已过期时应立即返回（duration_since 返回 Err）
        let past = SystemTime::now() - StdDuration::from_secs(1);
        sleep_until_wallclock(past).await;
        // 目标就在当下（剩余≈0）也应立即返回
        sleep_until_wallclock(SystemTime::now()).await;
    }

    #[tokio::test]
    async fn test_sleep_until_wallclock_slices_long_sleep() {
        // 2.5s 的目标最长总耗时不超过单片上限 60s + 少量余量；
        // 若未分片也仍能按时醒来（此处主要验证正常路径不悬挂、按时到期）
        let target = SystemTime::now() + StdDuration::from_millis(300);
        let start = TokioInstant::now();
        sleep_until_wallclock(target).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= TokioDuration::from_millis(250),
            "未到期不应提前返回"
        );
        assert!(elapsed < TokioDuration::from_secs(5), "到期后应及时返回");
    }
}
