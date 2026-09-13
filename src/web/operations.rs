//! Web 长操作生命周期登记：容量限制、取消令牌、暂停与 RAII 清理

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

type CancelAction = Box<dyn FnOnce(&str) + Send + 'static>;

/// 操作登记失败原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisterError {
    /// 登记器已暂停，通常表示卸载或资源回收正在进行
    Paused,
    /// 已达到并发容量
    CapacityReached,
    /// 相同操作 ID 已存在
    DuplicateId,
}

#[derive(Default)]
struct RegistryState {
    operations: HashMap<String, CancellationToken>,
    pause_depth: usize,
}

/// 可取消操作登记器
///
/// `capacity=None` 允许任意数量的不同操作；`Some(1)` 提供单飞语义。
/// 每次成功登记返回 [`OperationRegistration`]，其 Drop 会取消令牌并移除登记，
/// 因此正常返回、`?`、任务 abort 与 panic unwind 都走同一清理路径。
pub(crate) struct OperationRegistry {
    capacity: Option<usize>,
    state: Arc<Mutex<RegistryState>>,
}

impl OperationRegistry {
    /// 创建只允许一个在途操作的登记器
    pub(crate) fn exclusive() -> Self {
        Self::with_capacity(Some(1))
    }

    /// 创建指定并发上限的登记器（WE2-6：OCR 等每请求派生子进程的重资源
    /// 操作必须限并发；`concurrent()` 保留给确需并发的轻量操作）
    pub(crate) fn with_capacity(capacity: Option<usize>) -> Self {
        Self {
            capacity,
            state: Arc::new(Mutex::new(RegistryState::default())),
        }
    }

    /// 登记一个操作并返回 RAII guard
    pub(crate) fn register(
        &self,
        id: impl Into<String>,
    ) -> Result<OperationRegistration, RegisterError> {
        let id = id.into();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.pause_depth > 0 {
            return Err(RegisterError::Paused);
        }
        if state.operations.contains_key(&id) {
            return Err(RegisterError::DuplicateId);
        }
        if self
            .capacity
            .is_some_and(|capacity| state.operations.len() >= capacity)
        {
            return Err(RegisterError::CapacityReached);
        }

        let cancel = CancellationToken::new();
        state.operations.insert(id.clone(), cancel.clone());
        Ok(OperationRegistration {
            id,
            cancel,
            state: self.state.clone(),
            cancel_action: None,
            finished: false,
        })
    }

    /// 暂停新操作、取消并取出当前全部操作 ID
    ///
    /// 返回的 guard 存活期间新登记会得到 [`RegisterError::Paused`]；guard Drop
    /// 后恢复接收。嵌套暂停以计数处理，最外层 guard 释放后才真正恢复。
    pub(crate) fn pause_and_drain(&self) -> PausedOperations {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pause_depth = state.pause_depth.saturating_add(1);
        let operations = std::mem::take(&mut state.operations);
        let ids = operations.keys().cloned().collect();
        for token in operations.values() {
            token.cancel();
        }
        PausedOperations {
            ids,
            state: self.state.clone(),
        }
    }
}

/// 单个在途操作的 RAII 登记
pub(crate) struct OperationRegistration {
    id: String,
    cancel: CancellationToken,
    state: Arc<Mutex<RegistryState>>,
    cancel_action: Option<CancelAction>,
    finished: bool,
}

impl OperationRegistration {
    /// 获取供业务请求监听的取消令牌
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// 设置异常退出时执行的取消动作
    ///
    /// 正常结束应调用 [`Self::finish`]；Future 被 abort 或 panic unwind 时，Drop
    /// 会调用该动作，把本地生命周期取消继续传播到外部执行器。
    pub(crate) fn with_cancel_action(mut self, action: impl FnOnce(&str) + Send + 'static) -> Self {
        self.cancel_action = Some(Box::new(action));
        self
    }

    /// 标记业务操作已正常结算并释放登记，不触发外部取消动作
    pub(crate) fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for OperationRegistration {
    fn drop(&mut self) {
        self.cancel.cancel();
        if !self.finished {
            if let Some(action) = self.cancel_action.take() {
                action(&self.id);
            }
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .operations
            .remove(&self.id);
    }
}

/// 暂停期间被取出的操作集合
pub(crate) struct PausedOperations {
    ids: Vec<String>,
    state: Arc<Mutex<RegistryState>>,
}

impl PausedOperations {
    /// 返回暂停时存在的操作 ID
    pub(crate) fn ids(&self) -> &[String] {
        &self.ids
    }
}

impl Drop for PausedOperations {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pause_depth = state.pause_depth.saturating_sub(1);
    }
}

/// Web 层长操作登记集合
///
/// 由 [`crate::web::state::AppState`] 构造并注入 handler，避免路由模块持有全局
/// 可变状态，同时让 AI 单飞与 OCR 并发策略共享同一套生命周期实现。
pub(crate) struct WebOperations {
    ai_generation: OperationRegistry,
    ocr: OperationRegistry,
}

impl WebOperations {
    /// 构造 Web 长操作登记集合
    pub(crate) fn new() -> Self {
        Self {
            ai_generation: OperationRegistry::exclusive(),
            // WE2-6：OCR 每请求派生 Python/ddddocr 子进程，并发必须钳制为 1
            //（内存敏感；此前 capacity=None 无限并发会耗尽本地资源）
            ocr: OperationRegistry::exclusive(),
        }
    }

    /// AI 流式生成登记器（单飞）
    pub(crate) fn ai_generation(&self) -> &OperationRegistry {
        &self.ai_generation
    }

    /// OCR 识别登记器（允许并发，卸载时统一暂停与排空）
    pub(crate) fn ocr(&self) -> &OperationRegistry {
        &self.ocr
    }
}

impl Default for WebOperations {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_drop_cancels_and_releases_capacity() {
        let registry = OperationRegistry::exclusive();
        let first = registry.register("first").expect("首次登记");
        let token = first.cancellation_token();
        assert_eq!(
            registry.register("second").err(),
            Some(RegisterError::CapacityReached)
        );

        drop(first);

        assert!(token.is_cancelled());
        assert!(registry.register("second").is_ok());
    }

    #[test]
    fn panic_unwind_releases_exclusive_capacity() {
        let registry = OperationRegistry::exclusive();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _registration = registry.register("panic").expect("登记 panic");
            panic!("模拟生成任务 panic");
        }));

        assert!(panic.is_err());
        assert!(registry.register("recovered").is_ok());
    }

    #[test]
    fn cancel_action_only_runs_for_unfinished_registration() {
        let registry = OperationRegistry::with_capacity(None);
        let cancelled = Arc::new(Mutex::new(Vec::new()));

        let cancelled_on_drop = cancelled.clone();
        let unfinished = registry
            .register("unfinished")
            .expect("登记 unfinished")
            .with_cancel_action(move |id| cancelled_on_drop.lock().unwrap().push(id.to_string()));
        drop(unfinished);

        let cancelled_on_finish = cancelled.clone();
        registry
            .register("finished")
            .expect("登记 finished")
            .with_cancel_action(move |id| cancelled_on_finish.lock().unwrap().push(id.to_string()))
            .finish();

        assert_eq!(*cancelled.lock().unwrap(), vec!["unfinished".to_string()]);
    }

    #[test]
    fn concurrent_registry_tracks_distinct_ids() {
        let registry = OperationRegistry::with_capacity(None);
        let first = registry.register("first").expect("登记 first");
        let second = registry.register("second").expect("登记 second");
        assert_eq!(
            registry.register("first").err(),
            Some(RegisterError::DuplicateId)
        );
        drop((first, second));
    }

    #[test]
    fn pause_drains_cancels_and_reopens_on_drop() {
        let registry = OperationRegistry::with_capacity(None);
        let first = registry.register("first").expect("登记 first");
        let second = registry.register("second").expect("登记 second");
        let first_token = first.cancellation_token();
        let second_token = second.cancellation_token();

        let paused = registry.pause_and_drain();
        let mut ids = paused.ids().to_vec();
        ids.sort();
        assert_eq!(ids, ["first", "second"]);
        assert!(first_token.is_cancelled());
        assert!(second_token.is_cancelled());
        assert_eq!(
            registry.register("third").err(),
            Some(RegisterError::Paused)
        );

        drop(paused);
        assert!(registry.register("third").is_ok());
    }
}
