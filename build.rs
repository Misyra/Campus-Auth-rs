//! 嵌入 Windows 程序版本资源，并为 MSVC 助手启用现代公共控件。

fn main() {
    println!("cargo:rerun-if-changed=resources/windows/helper.manifest");
    println!("cargo:rerun-if-changed=resources/icons/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 字符串版本保留预发布标识，不能只依赖固定资源中的四段数字。
        // 版本资源按包级编译、随链接进入主程序与 helper 两个二进制，
        // OriginalFilename 以主程序 campus-auth.exe 为准（helper 上仅影响属性页展示）。
        let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/icons/app.ico");
        winresource::WindowsResource::new()
            .set("ProductVersion", env!("CARGO_PKG_VERSION"))
            .set("FileVersion", env!("CARGO_PKG_VERSION"))
            // 资源管理器属性页 / 任务管理器展示口径
            .set("FileDescription", "校园网自动认证工具")
            .set("OriginalFilename", "campus-auth.exe")
            // 与 Cargo.toml authors 同源，随元数据自动同步
            .set("CompanyName", env!("CARGO_PKG_AUTHORS"))
            // 多尺寸应用图标（16–256px，经典 DIB 条目）；由 frontend/public/logo.png 生成，
            // 生成脚本快照在 docs/reports/icons/make_app_ico.py（本地过程产物，不入库）：
            // uv run --with pillow python docs/reports/icons/make_app_ico.py \
            //     frontend/public/logo.png resources/icons/app.ico
            .set_icon(icon.to_str().expect("图标路径含非 UTF-8 字符"))
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
