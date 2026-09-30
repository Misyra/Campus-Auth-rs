//! 嵌入 Windows 程序版本资源，并为 MSVC 助手启用现代公共控件。

fn main() {
    println!("cargo:rerun-if-changed=resources/windows/helper.manifest");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 字符串版本保留预发布标识，不能只依赖固定资源中的四段数字。
        winresource::WindowsResource::new()
            .set("ProductVersion", env!("CARGO_PKG_VERSION"))
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            .compile()
            .expect("编译 Windows 版本资源失败");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/windows/helper.manifest");
        println!("cargo:rustc-link-arg-bin=campus-auth-helper=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=campus-auth-helper=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
