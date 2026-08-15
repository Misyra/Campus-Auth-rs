//! 原子计数器指标
//!
//! 用 `AtomicU64` 维护关键运行指标，通过 `Arc<Metrics>` 注入多个服务。各服务在关键路径上
//! `fetch_add` 递增，不做额外同步。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 关键运行指标（全部为 `AtomicU64`）
pub struct Metrics {
    /// 累计登录次数
    pub login_total: AtomicU64,
    /// 累计登录成功次数
    pub login_success_total: AtomicU64,
    /// 累计登录失败次数
    pub login_failure_total: AtomicU64,
    /// 累计登录取消次数
    pub login_cancel_total: AtomicU64,
    /// 累计探测次数
    pub probe_total: AtomicU64,
    /// 探测平均耗时（毫秒）
    pub probe_duration_ms_avg: AtomicU64,
    /// Worker 启动次数
    pub worker_spawn_total: AtomicU64,
    /// Worker 崩溃次数
    pub worker_crash_total: AtomicU64,
    /// 运行时长（秒）
    pub uptime_seconds: AtomicU64,
}

impl Metrics {
    /// 构造并以 `Arc<Metrics>` 返回，便于跨服务共享
    pub fn new() -> Arc<Metrics> {
        Arc::new(Metrics {
            login_total: AtomicU64::new(0),
            login_success_total: AtomicU64::new(0),
            login_failure_total: AtomicU64::new(0),
            login_cancel_total: AtomicU64::new(0),
            probe_total: AtomicU64::new(0),
            probe_duration_ms_avg: AtomicU64::new(0),
            worker_spawn_total: AtomicU64::new(0),
            worker_crash_total: AtomicU64::new(0),
            uptime_seconds: AtomicU64::new(0),
        })
    }

    /// 增加累计登录次数
    pub fn inc_login(&self) {
        self.login_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 增加累计登录成功次数
    pub fn inc_login_success(&self) {
        self.login_success_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 增加累计登录失败次数
    pub fn inc_login_failure(&self) {
        self.login_failure_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 增加累计登录取消次数
    pub fn inc_login_cancel(&self) {
        self.login_cancel_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次探测并更新平均耗时（毫秒）
    pub fn record_probe(&self, duration_ms: u64) {
        let total = self.probe_total.fetch_add(1, Ordering::Relaxed) + 1;
        let prev = self.probe_duration_ms_avg.load(Ordering::Relaxed);
        let new_avg = if total > 1 {
            (prev * (total - 1) + duration_ms) / total
        } else {
            duration_ms
        };
        self.probe_duration_ms_avg.store(new_avg, Ordering::Relaxed);
    }

    /// 增加 Worker 启动次数
    pub fn inc_worker_spawn(&self) {
        self.worker_spawn_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 增加 Worker 崩溃次数
    pub fn inc_worker_crash(&self) {
        self.worker_crash_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 设置运行时长（秒）
    pub fn set_uptime(&self, secs: u64) {
        self.uptime_seconds.store(secs, Ordering::Relaxed);
    }
}
