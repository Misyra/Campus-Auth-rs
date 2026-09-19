//! 基础冒烟测试：验证二进制可以启动并正确退出
use assert_cmd::Command;

#[test]
fn binary_prints_version() {
    // 断言跟随 crate 版本派生，而非写死具体号——写死的值会在版本提升时失效
    // （同 `updater_channels` 的 mock 版本口径）
    let expected = env!("CARGO_PKG_VERSION");
    let mut cmd = Command::cargo_bin("campus-auth").unwrap();
    cmd.arg("--version");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains(expected));
}
