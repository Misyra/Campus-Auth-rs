//! 更新操作所有权：调用方取消不能释放仍在执行的复制与提交任务。

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{UpdaterError, UpdaterService};

/// 守卫与独立任务共同存活，退出时才清理状态并释放操作槽位。
struct OperationGuard {
    service: UpdaterService,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        let _state = self
            .service
            .state_lock
            .lock()
            .unwrap_or_else(crate::utils::recover_lock);
        self.service.clear_update_progress();
        self.service
            .update_in_progress
            .store(false, Ordering::SeqCst);
    }
}

impl UpdaterService {
    /// 获取独占操作槽位，并把整个操作交给独立任务完成。
    pub(super) async fn run_operation<T, F, Fut>(&self, work: F) -> Result<T, UpdaterError>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, UpdaterError>> + Send + 'static,
    {
        let lock = Arc::clone(&self.operation_lock)
            .try_lock_owned()
            .map_err(|_| UpdaterError::UpdateInProgress)?;
        {
            let _state = self
                .state_lock
                .lock()
                .unwrap_or_else(crate::utils::recover_lock);
            if self.update_cancelled() {
                return Err(UpdaterError::Cancelled);
            }
            if self
                .update_in_progress
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(UpdaterError::UpdateInProgress);
            }
        }
        let guard = OperationGuard {
            service: self.clone(),
            _lock: lock,
        };
        let service = self.clone();
        // 丢弃 JoinHandle 不会取消任务，阻塞 I/O 的所有权也不会提前释放。
        tokio::spawn(async move {
            let _guard = guard;
            work(service).await
        })
        .await
        .map_err(|error| UpdaterError::OperationFailed(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 调用方中断后仍保持互斥，直到实际操作退出；随后允许重试。
    #[tokio::test]
    async fn caller_abort_preserves_operation_until_completion() {
        let dir = tempfile::tempdir().unwrap();
        let service = super::super::tests::make_service(dir.path()).await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn({
            let service = service.clone();
            async move {
                service
                    .run_operation(|_| async move {
                        started_tx.send(()).unwrap();
                        finish_rx.await.unwrap();
                        Ok(())
                    })
                    .await
            }
        });
        started_rx.await.unwrap();
        caller.abort();
        let _ = caller.await;
        assert!(matches!(
            service.run_operation(|_| async { Ok(()) }).await,
            Err(UpdaterError::UpdateInProgress)
        ));
        finish_tx.send(()).unwrap();
        let lock = service.operation_lock.lock().await;
        drop(lock);
        service.run_operation(|_| async { Ok(()) }).await.unwrap();
    }

    /// 取消必须等待在途操作，不允许它在取消完成后提交 pending。
    #[tokio::test]
    async fn cancellation_waits_for_operation_and_cleans_late_pending() {
        let dir = tempfile::tempdir().unwrap();
        let service = super::super::tests::make_service(dir.path()).await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let service = service.clone();
            async move {
                service
                    .run_operation(|service| async move {
                        started_tx.send(()).unwrap();
                        finish_rx.await.unwrap();
                        assert!(service.update_cancelled());
                        // 模拟旧提交者已经落下文件，清理也必须在操作之后发生。
                        std::fs::create_dir_all(service.base_path.join("update")).unwrap();
                        std::fs::write(service.base_path.join("update/pending.json"), b"{}")
                            .unwrap();
                        Ok(())
                    })
                    .await
            }
        });
        started_rx.await.unwrap();
        let cancel = tokio::spawn({
            let service = service.clone();
            async move { service.cancel_pending_update().await }
        });
        while !service.update_cancelled() {
            tokio::task::yield_now().await;
        }
        assert!(!cancel.is_finished());
        finish_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert!(cancel.await.unwrap());
        assert!(!service.has_pending_update());
    }
}
