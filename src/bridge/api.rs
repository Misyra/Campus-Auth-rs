//! Web 层消费的 Bridge 抽象：BridgeApi trait 与 BridgeSupervisor 实现
//!
//! handler 通过 `State<Arc<dyn BridgeApi>>` 提取依赖（debug/ocr/system 等
//! 路由），不触达 `state.container`，测试可注入内存实现。

use serde_json::Value;

use super::{BridgeError, BridgeSupervisor, IpcResponse};

/// Web 层消费的 Bridge 抽象（M1 细粒度 state：bridge 域）
///
/// handler 通过 `State<Arc<dyn BridgeApi>>` 提取依赖（debug/ocr/system 路由），
/// 不再触达 `state.container`，测试可注入内存实现（见 `web/routes/ocr.rs` 模块测试）。
#[async_trait::async_trait]
pub trait BridgeApi: Send + Sync {
    /// 执行 Worker 命令并等待响应。
    async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError>;
    /// 取消已注册 cancel_id 的在途命令。
    fn cancel(&self, cancel_id: &str);
    /// 带超时执行 Worker 命令（自生成 cancel_id，超时后可经 Cancel 立即打断）。
    async fn execute_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<IpcResponse, BridgeError>;
    /// 强制回收当前 Worker（kill 子进程并标记 Error，下次请求重新 spawn）。
    async fn force_recycle(&self);
    /// 归属感知的强制回收：仅当会话槽位**未被其他请求占用**时才强杀 Worker。
    ///
    /// 返回 `true` 表示已执行回收，`false` 表示因槽位被其他请求持有而跳过。
    /// 供登录会话在重试前回收「可能已损坏」的 Worker 使用——无条件回收会在
    /// 「登录与定时浏览器任务时间重叠」时杀掉对方正在使用的 Worker：对方在途请求
    /// 被 `trigger_all` 取消，以 `Cancelled`（用户可见「请求已取消」）中途失败，
    /// 根因却在另一条路径上，极难排查。
    ///
    /// `owner_cancel_id` 为调用方自己的 cancel_id；`None` 表示调用方无身份，
    /// 此时只要槽位被占用即视为「可能属于他人」而跳过。
    async fn force_recycle_if_unowned(&self, owner_cancel_id: Option<&str>) -> bool {
        // 内存 mock 无真实会话槽位，按「无冲突」处理
        let _ = owner_cancel_id;
        self.force_recycle().await;
        true
    }
    /// 是否存在存活 Worker 子进程。
    fn has_live_worker(&self) -> bool;
    /// 若 Worker 正在运行，则回收它以便下次请求按最新环境重新启动。
    async fn recycle_if_running(&self);
    /// 优雅关闭 Worker 与 Supervisor。
    async fn shutdown(&self);
    /// Worker 存活时最近一次健康检查上报的运行时 OCR 能力（任务 10）。
    ///
    /// `None` 表示 Worker 未存活或未上报能力，调用方回退文件探测。
    /// 默认实现返回 `None`，供内存 mock 等实现复用。
    fn runtime_ocr_capability(&self) -> Option<bool> {
        None
    }
    /// 是否存在活跃调试会话（登录类命令会被"Worker 忙"拒绝）。
    /// 默认实现返回 `false`，供内存 mock 等实现复用。
    fn debug_session_active(&self) -> bool {
        false
    }
    /// 会话槽位是否被在途请求占用（登录 / 定时浏览器任务 / 调试会话）。
    ///
    /// 供 OCR **卸载**在回收 Worker 前做前置检查。卸载必须回收持有 onnxruntime
    /// 的 Worker（Windows 不允许删除已加载的 DLL），但回收是破坏性的：若此时有
    /// 浏览器任务在途，`force_recycle` 会连带取消它（用户可见「请求已取消」，
    /// 根因却在 OCR 路径）。故调用方先经本方法判定，宁可拒绝本次操作也不要
    /// 静默摧毁他人。
    ///
    /// 注意：OCR **安装**（后台任务）同样调用 `recycle_if_running` 且未加本检查
    /// ——其触发概率低（一生一次的新用户配置期，通常无定时任务在跑），保持现状。
    /// 若后续要收敛，同一谓词可直接复用。
    ///
    /// 默认实现返回 `false`，供内存 mock 等实现复用。
    fn session_busy(&self) -> bool {
        false
    }
    /// 调试会话存续期最近一次截图的预览 URL（无会话或未截图时 `None`）。
    /// 默认实现返回 `None`，供内存 mock 等实现复用。
    fn last_screenshot_url(&self) -> Option<String> {
        None
    }
}

#[async_trait::async_trait]
impl BridgeApi for BridgeSupervisor {
    fn debug_session_active(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .debug_session_open
    }

    fn session_busy(&self) -> bool {
        // 槽位非空即视为有在途会话；debug_session_open 额外覆盖调试会话存续期
        // （该期间槽位常驻 Some(Debug)，上面一条已能覆盖，此处仅作防御）
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.current_cancel_id.is_some() || inner.debug_session_open
    }

    fn last_screenshot_url(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_screenshot_url
            .clone()
    }

    async fn execute(&self, method: &str, params: Value) -> Result<IpcResponse, BridgeError> {
        BridgeSupervisor::execute(self, method, params).await
    }

    fn cancel(&self, cancel_id: &str) {
        BridgeSupervisor::cancel(self, cancel_id);
    }

    async fn execute_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<IpcResponse, BridgeError> {
        BridgeSupervisor::execute_with_timeout(self, method, params, timeout).await
    }

    async fn force_recycle(&self) {
        BridgeSupervisor::force_recycle(self).await
    }

    async fn force_recycle_if_unowned(&self, owner_cancel_id: Option<&str>) -> bool {
        BridgeSupervisor::force_recycle_if_unowned(self, owner_cancel_id).await
    }

    fn has_live_worker(&self) -> bool {
        BridgeSupervisor::has_live_worker(self)
    }

    async fn recycle_if_running(&self) {
        if self.has_live_worker() {
            self.force_recycle().await;
        }
    }

    async fn shutdown(&self) {
        BridgeSupervisor::shutdown(self).await;
    }

    fn runtime_ocr_capability(&self) -> Option<bool> {
        BridgeSupervisor::runtime_ocr_capability(self)
    }
}
