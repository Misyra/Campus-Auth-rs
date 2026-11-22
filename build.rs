//! 嵌入 Windows 程序版本资源、发布清单签名公钥（可选），并为 MSVC 助手启用现代公共控件。

use base64::Engine as _;

fn main() {
    println!("cargo:rerun-if-changed=resources/windows/helper.manifest");
    println!("cargo:rerun-if-changed=resources/icons/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    embed_manifest_pubkey();
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

/// 发布清单签名公钥嵌入（T6-4，机制见 `src/updater/signature.rs` 模块 doc）
///
/// release 流程在启用清单签名（GitHub Secret `MANIFEST_SIGNING_KEY` 存在）时以
/// `CAMPUS_MANIFEST_SIGNING=1` 调用构建，此处读取仓内 `resources/manifest-pubkey`
/// 经 `rustc-env` 常量注入二进制；普通开发/CI 构建不设置该开关，二进制不嵌公钥，
/// 更新器对清单不做签名校验（与历史未签名清单兼容）。
/// 安全语义：公钥嵌入与清单签名必须同源同批——两者要么同时开启，要么都不做。
fn embed_manifest_pubkey() {
    println!("cargo:rerun-if-env-changed=CAMPUS_MANIFEST_SIGNING");
    println!("cargo:rerun-if-changed=resources/manifest-pubkey");
    if std::env::var("CAMPUS_MANIFEST_SIGNING").as_deref() != Ok("1") {
        return;
    }
    let key_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/manifest-pubkey");
    let key = std::fs::read_to_string(&key_path)
        .unwrap_or_else(|e| panic!("启用清单签名但缺少公钥文件 {}: {e}", key_path.display()));
    let key = key.trim();
    // base64（标准字母表带填充）的 32 字节 ed25519 公钥恒为 44 字符；此处快速
    // 拦截格式错误的仓内文件，让问题在构建期暴露而非运行期静默失效
    let valid = key.len() == 44
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=');
    if !valid {
        panic!("resources/manifest-pubkey 不是合法的 ed25519 公钥 base64（预期 44 字符）");
    }
    // 完整解析校验：44 字符 base64 只保证形态，不保证解码出的 32 字节是合法
    // ed25519 曲线点——损坏/被替换的公钥会通过形态检查，却在运行期解析失败而
    // fail-open 降级为不校验签名。构建期用 VerifyingKey::from_bytes 做完整
    // 解析，失败即构建报错（fail-closed），与运行期 embedded_verifying_key
    // 共用同一解析入口，不存在两套判定漂移
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(key)
        .unwrap_or_else(|e| panic!("resources/manifest-pubkey base64 解码失败: {e}"));
    let arr: [u8; 32] = bytes.try_into().unwrap_or_else(|v: Vec<u8>| {
        panic!(
            "resources/manifest-pubkey 应解码为 32 字节公钥，实际 {} 字节",
            v.len()
        )
    });
    if let Err(e) = ed25519_dalek::VerifyingKey::from_bytes(&arr) {
        panic!("resources/manifest-pubkey 不是合法的 ed25519 公钥（曲线点解析失败）: {e}");
    }
    println!("cargo:rustc-env=CAMPUS_MANIFEST_PUBKEY_B64={key}");
}
