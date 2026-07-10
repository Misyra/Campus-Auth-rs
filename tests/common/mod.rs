//! 测试共享工具：临时配置、mock server 等
use tempfile::TempDir;

/// 创建带基本配置的临时目录供测试使用
pub fn setup_test_env() -> TempDir {
    let dir = TempDir::new().unwrap();
    let config_dir = dir.path().join("config").join("profiles");
    std::fs::create_dir_all(&config_dir).unwrap();
    let tasks_dir = dir.path().join("tasks").join("browser");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    dir
}
