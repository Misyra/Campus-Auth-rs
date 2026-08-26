//! 基础冒烟测试：验证二进制可以启动并正确退出
use assert_cmd::Command;

#[test]
fn binary_prints_version() {
    let mut cmd = Command::cargo_bin("campus-auth").unwrap();
    cmd.arg("--version");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("5.0.0"));
}
