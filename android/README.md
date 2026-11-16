# Android 壳工程（认证喵）

Kotlin 薄壳：前台服务（FGS specialUse）常驻承载 Rust 认证核心，MainActivity 用系统 WebView 加载本机 axum 服务（`http://127.0.0.1:<port>`）展示既有 Vue 前端；Rust 核心编译为 `libcampus_auth.so` 经 `jniLibs` 打进 APK。

## 目录结构

```
android/
├── settings.gradle.kts / build.gradle.kts / gradle.properties
├── gradle/wrapper/gradle-wrapper.properties   # Gradle 8.9（wrapper jar 不提交，见下）
└── app/
    ├── build.gradle.kts                       # minSdk 26 / targetSdk 35 / 仅依赖 core-ktx
    └── src/main/
        ├── AndroidManifest.xml                # 权限 + Activity + specialUse 服务 + BootReceiver
        ├── java/rs/campusauth/app/            # AuthNative / MainActivity / AuthForegroundService / BootReceiver
        ├── res/                               # 主题 / 文案 / 网络安全配置
        └── jniLibs/                           # cargo-ndk 产出的 libcampus_auth.so（构建前拷入）
```

## 构建步骤

前置要求：JDK 17、Android SDK（compileSdk 35）、本机 Gradle 8.9+（或任意能运行的 Gradle，见第 2 步）。

1. **产出 Rust 动态库**：Rust 侧需提供 JNI 入口（契约见下节）并以 `cdylib` 方式编译（本次壳工程不动 `Cargo.toml`，crate-type 与 JNI 模块为 Rust 侧后续工作）。示意命令（仓库根执行）：

   ```bash
   cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release
   ```

   产物 `libcampus_auth.so` 会被 cargo-ndk 直接放进 `android/app/src/main/jniLibs/arm64-v8a/`；目录不存在时先 `mkdir -p`。需要其他 ABI 时追加 `-t` 参数。

2. **生成 Gradle Wrapper**：`gradle-wrapper.properties` 已固定 8.9，但 **wrapper jar 不提交入库**。首次构建前在本目录用本机 Gradle 生成一次（会产出 `gradlew` / `gradlew.bat` / `gradle-wrapper.jar`，均不入库）：

   ```bash
   cd android
   gradle wrapper --gradle-version 8.9
   ```

3. **构建 APK**：

   ```bash
   cd android
   ./gradlew assembleDebug        # 产物 app/build/outputs/apk/debug/app-debug.apk
   ```

   本机未配 Android SDK 时需先设置 `ANDROID_HOME`（或写 `android/local.properties`，该文件已 gitignore）。

4. **安装运行**：`adb install app/build/outputs/apk/debug/app-debug.apk`。

> **release 未配置签名**：`assembleRelease` 产出的是未签名 APK。P1 再加 `signingConfig`（含密钥托管方案），此前 release 分发请自行用 `apksigner` 签名。

## JNI 契约（与 Rust 侧约定死，不得改名）

```kotlin
package rs.campusauth.app

object AuthNative {
    init { System.loadLibrary("campus_auth") }
    external fun nativeInit(baseDir: String): Int   // 返回端口，0 = 失败
    external fun nativePort(): Int
    external fun nativeStatus(): String             // JSON
    external fun nativeStop(): Int
}
```

Rust 实现侧须知：

- 这些是 Kotlin `object` 的**实例方法**（非 `@JvmStatic`），JNI 符号按实例方法约定：`Java_rs_campusauth_app_AuthNative_<方法名>(JNIEnv*, jobject, ...)`；
- `nativeInit` 必须幂等：核心已运行时直接返回现有端口，不得重复起服务（Activity 重建会再次调用）；
- `nativePort` / `nativeStatus` / `nativeStop` 必须容忍「未初始化」状态（返回 0 / 占位 JSON / 0）——开机自启路径会先拉起前台服务轮询状态，`nativeInit` 要等用户打开 App 才执行；
- `baseDir` 为应用私有目录（`filesDir`），核心的运行时目录布局（config / tasks / events 等）应挂在其下。

## 前台服务（specialUse）与保活

- 服务声明 `android:foregroundServiceType="specialUse"` + `FOREGROUND_SERVICE_SPECIAL_USE` 权限，并在 service 内以 `PROPERTY_SPECIAL_USE_FGS_SUBTYPE` property 声明具体用途——targetSdk 34+（Android 14+）的强制要求，见官方文档：https://developer.android.com/develop/background-work/services/fg/service-types#special-use
- 通知渠道 `auth_guard`（IMPORTANCE_LOW 静默），状态文案 P0 为 5s 粗轮询 `nativeStatus()`，TODO(P1) 改 WS 订阅。
- **Doze / 电池优化**：清单已申请 `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`（弹窗确认流程 TODO(P1) 接入），也可手动加白名单：

  ```bash
  adb shell dumpsys deviceidle whitelist +rs.campusauth.app
  ```

- **国产 ROM**：各厂商的「自启动 / 后台管控 / 省电策略」会杀常驻服务，引导用户按厂商逐项设置；现状与对策见 https://dontkillmyapp.com （保活指引文案占位于 `strings.xml` 的 `keepalive_hint`，TODO(P2) 设置 UI）。

## 已知限制（P0）

- **核心初始化只在 MainActivity**：开机自启仅拉起服务壳，通知显示「核心初始化中」直到用户打开一次 App；TODO(P2) 评估把 `nativeInit` 下沉到服务侧（需 Rust 侧保证幂等）。
- **`#auth=` fragment 语义偏差**：前端该 fragment 的既有契约是 bootstrap 秘钥（经 `/api/auth/exchange` 换取），而会话 token 恰好也是 64 位 hex，放进 fragment 会被前端先误判为秘钥发起一次必败的 exchange（401），随后自动回落 local-token 免引导兜底——行为正确但多一次往返；TODO(P1) 与前端定稿 token 注入的正式通道。
- 通知小图标临时借用平台 `android.R.drawable.stat_notify_sync`，TODO(P1) 换正式应用图标（含 launcher icon）。
- `nativeStatus()` 的 JSON 字段未定稿，通知文案 P0 尽力取 `status` 字段、取不到原样截断展示。
- 未做四端同版机制（与 `Cargo.toml` / `frontend/package.json` / `openapi.json` 的版本联动），TODO(P3)。
