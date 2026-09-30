//! 在隔离复制的测试进程中实际运行启动更新兜底，验证完整分发与运行态保留。

use std::path::PathBuf;
use std::sync::Arc;

use campus_auth::updater::{PendingUpdate, UpdaterService};

/// 子测试只在复制到隔离目录后执行替换，绝不操作 Cargo 或用户正在运行的程序。
#[tokio::test]
#[ignore = "仅由父测试在隔离复制进程中运行"]
async fn recovery_child() {
    let base = std::env::var_os("CAMPUS_AUTH_RECOVERY_FIXTURE").expect("缺少隔离测试目录");
    let base = PathBuf::from(base);
    let executable = std::env::current_exe().unwrap();
    assert_eq!(executable.parent().unwrap(), base.as_path());
    assert!(!base.join("Cargo.toml").exists());
    let worker = base.join("python_worker");
    std::fs::create_dir_all(worker.join(".venv")).unwrap();
    std::fs::write(worker.join("pyproject.toml"), b"old-project").unwrap();
    std::fs::write(worker.join(".venv/keep"), b"runtime").unwrap();
    let staging = base.join("update/staging");
    let extracted = staging.join("extracted");
    std::fs::create_dir_all(extracted.join("python_worker/.venv")).unwrap();
    std::fs::create_dir_all(extracted.join("resources")).unwrap();
    std::fs::create_dir_all(extracted.join("docs")).unwrap();
    std::fs::write(
        extracted.join("python_worker/pyproject.toml"),
        b"new-project",
    )
    .unwrap();
    std::fs::write(
        extracted.join("python_worker/.venv/keep"),
        b"do-not-install",
    )
    .unwrap();
    std::fs::write(extracted.join("resources/new.txt"), b"resource").unwrap();
    std::fs::write(extracted.join("docs/new.txt"), b"guide").unwrap();
    std::fs::write(
        extracted.join(campus_auth::uninstall::helper_exe_name()),
        b"fixture-helper",
    )
    .unwrap();
    let staged_exe = extracted.join(campus_auth::uninstall::main_exe_name());
    // 使用同一可运行测试程序作为替换内容，分别演练新版本替换和提交中断后的同版本修复。
    std::fs::copy(&executable, &staged_exe).unwrap();
    let pending = PendingUpdate {
        version: if std::env::var_os("CAMPUS_AUTH_RECOVERY_SAME_VERSION").is_some() {
            env!("CARGO_PKG_VERSION").into()
        } else {
            "999.0.0".into()
        },
        staging_dir: staging.to_string_lossy().into_owned(),
        target_exe: executable.to_string_lossy().into_owned(),
        worker_target_dir: worker.to_string_lossy().into_owned(),
        original_args: vec![],
        sha256: campus_auth::utils::io::file_sha256(&staged_exe).unwrap(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    campus_auth::utils::io::atomic_write_json(&base.join("update/pending.json"), &pending).unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let config = campus_auth::config::ConfigService::new(base.clone(), tx)
        .await
        .unwrap();
    let service = UpdaterService::new(
        config,
        Arc::new(campus_auth::status::StatusManager::new()),
        base.clone(),
    );
    if std::env::var_os("CAMPUS_AUTH_RECOVERY_BLOCK_SYNC").is_some() {
        std::fs::write(base.join("resources"), b"blocked-directory").unwrap();
        assert!(service.apply_pending_on_startup().await.is_err());
        assert!(service.has_pending_update());
        assert!(staged_exe.exists());
        std::fs::remove_file(base.join("resources")).unwrap();
    }
    assert!(service.apply_pending_on_startup().await.unwrap());
    assert_eq!(
        std::fs::read(worker.join("pyproject.toml")).unwrap(),
        b"new-project"
    );
    assert_eq!(
        std::fs::read(worker.join(".venv/keep")).unwrap(),
        b"runtime"
    );
    assert!(
        worker
            .join(campus_auth::environment::RESYNC_MARKER)
            .exists()
    );
    assert_eq!(
        std::fs::read(base.join("resources/new.txt")).unwrap(),
        b"resource"
    );
    assert_eq!(std::fs::read(base.join("docs/new.txt")).unwrap(), b"guide");
    assert_eq!(
        std::fs::read(base.join(campus_auth::uninstall::helper_exe_name())).unwrap(),
        b"fixture-helper"
    );
    assert!(!base.join("update/pending.json").exists());
    assert!(!staging.exists());
}

/// 自替换只能在复制的进程中演练，父测试负责检查子进程完成情况。
#[test]
fn startup_recovery_syncs_complete_distribution() {
    run_recovery(false, false);
}

/// 同步失败必须保留完整包，解除阻碍后可重试。
#[test]
fn startup_recovery_retains_staging_on_sync_failure() {
    run_recovery(true, false);
}

/// 主程序已提交且摘要一致时，恢复尚未同步的同版本分发文件。
#[test]
fn startup_recovery_repairs_same_version_distribution() {
    run_recovery(false, true);
}

fn run_recovery(block_sync: bool, same_version: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let executable = fixture.path().join(if cfg!(windows) {
        "recovery.exe"
    } else {
        "recovery"
    });
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let mut command = std::process::Command::new(&executable);
    command
        .args(["--exact", "recovery_child", "--ignored", "--nocapture"])
        .env("CAMPUS_AUTH_RECOVERY_FIXTURE", fixture.path());
    if block_sync {
        command.env("CAMPUS_AUTH_RECOVERY_BLOCK_SYNC", "1");
    }
    if same_version {
        command.env("CAMPUS_AUTH_RECOVERY_SAME_VERSION", "1");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
