//! Android JNI 薄层（P0 骨架）：把 Rust 控制平面（ServiceContainer + Engine + Axum）
//! 以 libcampus_auth.so 形态交给安卓壳（Kotlin）通过 JNI 调起
//!
//! - Kotlin 侧约定（与安卓端约定死，不得改名）：`package rs.campusauth.app`、
//!   object `AuthNative`、`System.loadLibrary("campus_auth")`。
//! - 本模块整体仅在 `target_os = "android"` 下编译（cfg 门在 `lib.rs`），
//!   host 构建与测试天然跳过。
//! - P0 范围：nativeInit 跑通「目录预建 → 日志 → Axum 预绑定 → 容器 → startup()
//!   后台任务 → Axum serve → Engine 启动动作」，启动语义为桌面 launcher 完整模式
//!   的子集；实例锁 / Engine 崩溃恢复（engine_watch）/ 定时自重启 / 后台更新检查
//!   均未纳入，见各 TODO(P1)。
//! - JNI 面：nativeInit / nativePort / nativeStatus / nativeStop /
//!   nativeBootstrapSecret（P2-11 起 WebView 界面的 `#auth=` 引导秘钥来源）。
//! - 生命周期（P2-12 收敛）：`init → 运行 → stop` 单程语义改为**可重启**——
//!   nativeStop 异步受理（取走实例转交 runtime 后立即返回，不在 FGS onDestroy
//!   的主线程上同步等待全链优雅关闭），再次 nativeInit 先等上一轮关闭彻底完成
//!   再重新引导；tracing 全局 subscriber 无法二次注册，日志层跨实例沿用
//!   （见 [`bootstrap_android`] 日志小节的二次初始化防护注释）。
//! - 失败策略：所有 JNI 入口不 panic、不跨边界抛异常；失败返回 0 / null，并尽量
//!   eprintln 到 stderr（Android 上 stderr 可映射进 logcat）。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::LevelFilter;

use crate::app::{self, AxumServeHandle};
use crate::config::schema::StartupAction;
use crate::container::{ServiceContainer, StartupHandles};
use crate::engine::EngineCommand;

/// JNI 侧 stderr 输出前缀（logcat 中检索用；tracing 控制台层同写 stderr）
const FFI_LOG_TAG: &str = "[campus-auth-ffi]";

/// nativeStatus 在服务未就绪（未初始化 / 序列化失败）时的占位响应
// TODO(P1): 细化为带 error 字段的显式占位，避免 {"ok":true} 与真实成功混淆
const STATUS_PLACEHOLDER: &str = r#"{"ok":true}"#;

// ============================================================
// 进程级状态
// ============================================================

/// Android 实例运行态：nativeInit 成功后驻留 [`INSTANCE`]，nativeStop 取出并逆序关闭
struct AndroidInstance {
    /// 运行时 base 目录（安卓应用 filesDir）：nativeBootstrapSecret 等二次查询的来源
    base_path: std::path::PathBuf,
    /// Axum 实际监听端口（prepare_axum_listener 绑定结果；默认端口被占时由内核分配回退）
    port: u16,
    /// 应用级关闭令牌（容器内 uptime / 登录 shutdown 均为其 child，取消即联动传播）
    shutdown_token: CancellationToken,
    /// startup() 返回的后台服务句柄（Scheduler / Bridge / EventSink / 推送通知）
    startup: Option<StartupHandles>,
    /// Axum serve 任务句柄（含优雅关闭信号与实际端口）
    axum: Option<AxumServeHandle>,
    /// 服务容器（nativeStatus 读快照、nativeStop 停 Engine 的引用来源）
    container: Option<Arc<ServiceContainer>>,
}

/// 全进程唯一的 tokio multi-thread runtime：容器 / 服务的构造与关闭在其上 block_on
///
/// OnceLock 永不回收是**有意取舍**：库随进程存活，static 中的值没有可触发的
/// Drop 时机，进程退出时由内核统一回收。运行中的服务各自持有 `Arc`，本 static
/// 只存 runtime 句柄，不持有服务。
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// 实例槽位：Some = 已初始化（幂等依据），None = 未初始化 / 已停止 / 启动失败
static INSTANCE: Mutex<Option<AndroidInstance>> = Mutex::new(None);

/// 取实例槽位（poison 恢复：持锁线程 panic 后槽位内容仍以实际状态为准）
///
/// nativeInit 全程持锁完成初始化，天然保证并发 / 重复调用的幂等：
/// 后到者在锁上排队，进锁后读到 Some 直接返回既有端口。
/// nativeStop **不得**经本函数排队取锁——它在 FGS onDestroy 的主线程调用，
/// 若初始化进行中（锁被后台初始化线程持有数秒），排队等待即主线程 ANR；
/// nativeStop 改用 `try_lock` 放弃停止请求（见其实现）。
fn lock_instance() -> MutexGuard<'static, Option<AndroidInstance>> {
    INSTANCE.lock().unwrap_or_else(|e| e.into_inner())
}

/// nativeStop 异步受理后尚未完成的关闭任务句柄
///
/// 再次 nativeInit 前先取走并等待完成：新旧实例对默认端口（50721 回环）与
/// 运行时目录的竞态在此显式收敛，而不是依赖「后台时序恰好错开」。
/// 等待发生在 JNI 调用线程（Kotlin 侧本就要求后台线程调 nativeInit），
/// 不阻塞 Android 主线程。
static PENDING_SHUTDOWN: Mutex<Option<tokio::task::JoinHandle<()>>> = Mutex::new(None);

/// 日志广播通道的全进程唯一 sender（跨实例复用）
///
/// tracing 全局 subscriber 只能注册一次（stop 后再次 nativeInit 无法重建
/// BroadcastLayer），但 `log_broadcast_tx()` 每次调用都新建通道：若二次引导
/// 时另起通道，新 Axum 的 WS 日志层会订阅到无人发送的新通道（旧
/// BroadcastLayer 仍往第一代通道推）。经本 static 固定同一 sender，新 Axum
/// 的订阅与旧 BroadcastLayer 的推送恒在同一条通道上汇合。
static LOG_BROADCAST_TX: OnceLock<tokio::sync::broadcast::Sender<crate::logging::LogEntry>> =
    OnceLock::new();

// ============================================================
// JNI 入口（签名与 Kotlin 侧 rs.campusauth.app.AuthNative 逐字对应）
// ============================================================

/// 初始化控制平面并启动 Web 服务，返回监听端口（0 = 失败）
///
/// - `base_dir`：安卓应用 filesDir 路径，作为运行时目录布局的 base（写入
///   `CAMPUS_AUTH_BASE_DIR` 环境变量 + 显式传入容器构造）。
/// - 幂等：已初始化时直接返回既有端口，不重复初始化（重复调用携带的
///   `base_dir` 被忽略）。
/// - 可重启（P2-12）：nativeStop 后实例槽位已复位，本入口可再次引导新实例；
///   动手前先等上一轮异步关闭彻底完成（[`PENDING_SHUTDOWN`]），避免新旧
///   实例争默认端口 / 双写运行时目录。
#[unsafe(no_mangle)]
pub extern "system" fn Java_rs_campusauth_app_AuthNative_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    base_dir: JString,
) -> jint {
    // JNI 字符串只在调用线程上取值，转为 owned Rust String 后再进入异步世界
    let base_dir = match env.get_string(&base_dir) {
        Ok(s) => s.to_string_lossy().to_string(),
        Err(e) => {
            eprintln!("{FFI_LOG_TAG} nativeInit 读取 base_dir 失败: {e}");
            return 0;
        }
    };
    if base_dir.trim().is_empty() {
        eprintln!("{FFI_LOG_TAG} nativeInit base_dir 为空，拒绝初始化");
        return 0;
    }

    // 全程持锁：并发 / 重复调用在锁上排队，先到者完成初始化，后到者读到 Some 幂等返回
    let mut slot = lock_instance();
    if let Some(instance) = slot.as_ref() {
        return jint::from(instance.port);
    }

    // SAFETY（edition 2024 起 set_var 为 unsafe）：进程环境变量的并发读写是 UB。
    // 本函数是库侧首个入口，写入发生在此前没有任何 Rust 线程读取过环境变量的
    // 时刻；tokio runtime 此后才创建，配置与路径解析均在其上串行读取，与本次
    // 写入经 nativeInit 的阻塞返回建立 happens-before。
    // 必须先于一切路径解析执行：`CAMPUS_AUTH_BASE_DIR` 是 base 目录的环境变量
    // 来源（utils::paths 侧接入），Worker 工程目录等旁路解析依赖它已就位；
    // 容器构造仍显式传 base_path，env 变量是给旁路兜底而非唯一途径。
    unsafe { std::env::set_var("CAMPUS_AUTH_BASE_DIR", &base_dir) };

    let runtime = match RUNTIME.get() {
        Some(rt) => rt,
        None => match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => RUNTIME.get_or_init(|| rt),
            Err(e) => {
                eprintln!("{FFI_LOG_TAG} nativeInit 创建 tokio runtime 失败: {e}");
                return 0;
            }
        },
    };

    // 可重启生命周期的收敛点：上一轮 nativeStop 已把实例取走并转交 runtime 异步
    // 关闭，这里先等它彻底完成再引导新实例（默认端口释放 / 目录写入静默）。
    // 等待期间持有实例槽位锁——关闭任务不触碰槽位（实例已 move 进任务），无死锁；
    // nativeStop 侧用 try_lock，不会被本等待卡住主线程
    if let Some(pending) = PENDING_SHUTDOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        match runtime.block_on(pending) {
            Ok(()) => {}
            Err(e) => {
                // JoinHandle Err = 关闭任务内 panic：旧实例的清理已尽力执行，
                // 新实例照常引导（端口被残占时 prepare_axum_listener 内部回退）
                eprintln!("{FFI_LOG_TAG} 等待上一轮实例关闭失败（继续初始化）: {e}");
            }
        }
    }

    // block_on 在 JNI 调用线程上同步完成启动（耗时数秒量级，Kotlin 侧应放后台线程）
    let base_path = Path::new(&base_dir).to_path_buf();
    let boot = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(bootstrap_android(&base_path))
    }));
    match boot {
        Ok(Ok(instance)) => {
            let port = instance.port;
            *slot = Some(instance);
            tracing::info!(port, "Android JNI 初始化完成");
            jint::from(port)
        }
        Ok(Err(e)) => {
            eprintln!("{FFI_LOG_TAG} nativeInit 启动失败: {e:#}");
            // 不释放日志 guard（P2-12，与 P0 单程语义不同）：tracing 全局
            // subscriber 一旦注册即不可二次注册，释放 guard 会关闭仍在挂载的
            // 文件层写线程，此后重试实例的文件日志永久静默；appender 后台
            // 线程空转的代价远小于文件日志不可恢复
            0
        }
        Err(_) => {
            eprintln!("{FFI_LOG_TAG} nativeInit 启动过程发生 panic");
            0
        }
    }
}

/// 查询当前监听端口（未初始化返回 0）
#[unsafe(no_mangle)]
pub extern "system" fn Java_rs_campusauth_app_AuthNative_nativePort(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    match lock_instance().as_ref() {
        Some(instance) => jint::from(instance.port),
        None => 0,
    }
}

/// 状态 JSON 快照（`StatusSnapshot` 的 serde 序列化，与 `GET /api/status` 同源）
///
/// 数据源为 [`StatusManager`](crate::status::StatusManager) 的 watch 快照
/// （`borrow()` 同步读取，无需进 runtime）。未初始化 / 序列化失败时返回
/// `{"ok":true}` 占位。
#[unsafe(no_mangle)]
pub extern "system" fn Java_rs_campusauth_app_AuthNative_nativeStatus(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let json = match lock_instance()
        .as_ref()
        .and_then(|instance| instance.container.as_ref())
    {
        Some(container) => {
            // watch::Receiver::borrow 同步取最新快照；序列化在本线程完成
            serde_json::to_string(&container.status.borrow()).unwrap_or_else(|e| {
                eprintln!("{FFI_LOG_TAG} nativeStatus 序列化快照失败: {e}");
                STATUS_PLACEHOLDER.to_string()
            })
        }
        None => STATUS_PLACEHOLDER.to_string(),
    };
    match env.new_string(json.as_str()) {
        // into_raw 把 local reference 交给 JVM 管理（本函数返回值即该引用）
        Ok(s) => s.into_raw(),
        Err(e) => {
            eprintln!("{FFI_LOG_TAG} nativeStatus 创建 jstring 失败: {e}");
            std::ptr::null_mut()
        }
    }
}

/// 读取界面引导（bootstrap）秘钥（`config/.auth_bootstrap` 内容），未初始化 /
/// 文件缺失 / 损坏时返回 null
///
/// P2-11：安卓构建已关闭回环 local-token 免引导端点（`web::auth` 的
/// `local_token_allowed` 恒 false——Android 的 127.0.0.1 为全设备应用共享，
/// 免引导等于向同设备任意第三方应用发放 token），WebView 界面的 token 引导
/// 收敛到既有 `#auth=<bootstrap>` fragment 通道：Kotlin 在 nativeInit 成功后
/// 调本入口取秘钥拼 `http://127.0.0.1:<port>/#auth=<secret>`，前端 exchange
/// 握手零改动。与桌面托盘/launcher 的 `read_bootstrap_file` 同源。
#[unsafe(no_mangle)]
pub extern "system" fn Java_rs_campusauth_app_AuthNative_nativeBootstrapSecret(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let secret = lock_instance()
        .as_ref()
        .and_then(|instance| crate::web::auth::read_bootstrap_file(&instance.base_path));
    let Some(secret) = secret else {
        // 未初始化 / 秘钥文件缺失或损坏：返回 null，Kotlin 侧回退不带 fragment
        // 的普通 URL（界面会显示引导失败态）
        return std::ptr::null_mut();
    };
    match env.new_string(secret.as_str()) {
        // into_raw 把 local reference 交给 JVM 管理（本函数返回值即该引用）
        Ok(s) => s.into_raw(),
        Err(e) => {
            eprintln!("{FFI_LOG_TAG} nativeBootstrapSecret 创建 jstring 失败: {e}");
            std::ptr::null_mut()
        }
    }
}

/// 请求停止控制平面（1 = 已受理或本就未运行，0 = 受理失败）
///
/// **异步受理**（P2-12）：取走实例槽位后把全链优雅关闭转交 tokio runtime，
/// 立即返回，绝不在调用线程上同步等待——调用方是 FGS `onDestroy` 的主线程，
/// 全链关闭预算最坏 ~29s（Scheduler 5s + Engine 5s + Bridge 8s + 通知/
/// EventSink 各 3s + Axum 5s），同步 `block_on` 必然 ANR。Kotlin 契约
/// 「非 0 = 已请求停止」自此与实现一致。
///
/// 关闭顺序对齐 `launcher::shutdown::graceful_shutdown`（去除托盘 / 实例锁 /
/// 更新 helper）：先取消应用级关闭令牌，再 Scheduler → Engine → Bridge →
/// 通知 / EventSink → Axum → 端口文件清理。
///
/// 可重启生命周期的另一半：槽位在受理时即复位，再次 nativeInit 会先等
/// [`PENDING_SHUTDOWN`] 的关闭任务完成后重新引导。初始化进行中（槽位锁被
/// 后台初始化线程持有）时放弃本次停止请求返回 0——在主线程排队等锁同样
/// 是 ANR，核心随后照常完成初始化，由下次停止或系统杀进程收敛。
#[unsafe(no_mangle)]
pub extern "system" fn Java_rs_campusauth_app_AuthNative_nativeStop(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    let Some(runtime) = RUNTIME.get() else {
        // runtime 尚未创建：无服务可停，幂等成功
        return 1;
    };
    // try_lock：绝不在主线程排队等初始化锁（见函数级注释）
    let mut slot = match INSTANCE.try_lock() {
        Ok(guard) => guard,
        // poison 恢复与 lock_instance 同口径：以槽位实际状态为准
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            eprintln!("{FFI_LOG_TAG} nativeStop：核心正在初始化，跳过本次停止请求");
            return 0;
        }
    };
    let Some(mut instance) = slot.take() else {
        // 槽位为空：未初始化或已停止，幂等成功
        return 1;
    };
    // 受理即转交：关闭链在 tokio worker 上执行，句柄留给下一次 nativeInit 等待
    let handle = runtime.spawn(async move {
        shutdown_android(&mut instance).await;
        tracing::info!("Android JNI 实例已关闭");
    });
    *PENDING_SHUTDOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(handle);
    1
}

// ============================================================
// 启动 / 关闭编排（内部实现）
// ============================================================

/// Android 启动编排（在 RUNTIME 上执行）：桌面 launcher 完整模式的 P0 子集
///
/// 步骤 1~6 与 `launcher::run_after_logging` 的完整模式路径同序同语义：
/// 目录预建 → 日志初始化 → Axum 预绑定（先于容器，端口不可用不启动整套服务）→
/// 容器构造（内含 startup() 后台任务）→ startup_action 派发 → Axum serve。
///
/// TODO(P1)：迁入 launcher 新增 Android 运行模式，复用实例锁 / 配置启动期轻量读 /
/// engine_watch 崩溃恢复 / 定时自重启 / 后台更新检查；本函数届时只做转发。
async fn bootstrap_android(base_path: &Path) -> anyhow::Result<AndroidInstance> {
    // 1. 预建运行时目录（幂等；失败先于一切服务构造抛出）
    crate::utils::paths::ensure_runtime_dirs(base_path)
        .map_err(|e| anyhow::anyhow!("预建运行时目录失败: {e}"))?;

    // 2. 日志子系统（控制台层 stderr + 文件层按日轮转 + 广播层 WS 推送）
    //    P0 固定 schema 默认值 INFO / 保留 7 天 / 开文件——launcher 的启动期
    //    轻量配置读在 launcher::config 内部不可复用，TODO(P1) 对齐
    //    settings.logging 的真实值（容器构造后可经 reload_log_level 热修正）
    //
    //    二次初始化防护（可重启生命周期的配套）：tracing 全局 subscriber 只能
    //    注册一次，stop 后再次引导时 init_logging 的 try_init 必失败——照常
    //    调用不但无益（新三层永不挂载），stash_log_guard 的覆盖式存入还会把
    //    上一代 guard 挤出 drop，关闭仍在挂载的旧文件层写线程，此后所有实例
    //    的文件日志静默丢行。已初始化时跳过重复 init，旧三层继续服务；广播
    //    通道经 LOG_BROADCAST_TX 跨实例固定同一 sender，新 Axum 的 WS 日志层
    //    与旧 BroadcastLayer 恒在同一条通道上汇合
    let log_tx = LOG_BROADCAST_TX
        .get_or_init(crate::logging::log_broadcast_tx)
        .clone();
    if crate::logging::logging_initialized() {
        tracing::info!(
            "日志系统已随上一代实例初始化，跨实例沿用（tracing 全局 subscriber 单次注册）"
        );
    } else {
        let log_guard =
            crate::logging::init_logging(base_path, log_tx.clone(), LevelFilter::INFO, 7, true);
        crate::logging::stash_log_guard(log_guard);
    }

    // 3. 预绑定 Axum 监听器（对齐 launcher 完整模式：端口等不可恢复错误不启动
    //    Engine / Scheduler / Bridge 后立刻整套关闭；回环绑定凭据门禁恒放行，
    //    端口被占时的系统分配回退由其内部处理）
    let bind_gate = app::BindCredentialGate {
        effective_password: String::new(),
        allow_unauthenticated_bind: false,
    };
    let prepared = app::prepare_axum_listener(app::DEFAULT_PORT, Some("127.0.0.1"), &bind_gate)
        .await
        .map_err(|e| anyhow::anyhow!("Web 控制台端口准备失败: {e}"))?;
    let port = prepared.port;

    // 4. 服务容器（Engine 已在 new() 内 spawn；startup() 启动 EventSink /
    //    推送通知 / Scheduler / Bridge 后台任务）。令牌在容器构造前创建：
    //    任一步失败即取消，联动回收已 spawn 的任务，不留游离后台 task
    let shutdown_token = CancellationToken::new();
    let started = async {
        let (container, startup) = ServiceContainer::new(base_path, shutdown_token.clone()).await?;

        // 5. startup_action 派发（对齐 launcher::modes::apply_startup_action 的
        //    Monitor 分支；LoginOnce 的「成功即取消令牌退出」语义与 Android
        //    常驻进程形态冲突，P0 不支持——TODO(P1) 决议 Android 下的该语义）
        match container.config.load_settings().global.app.startup_action {
            StartupAction::Monitor => {
                if let Err(e) = container.engine.dispatch(EngineCommand::Start).await {
                    tracing::warn!("按 startup_action=monitor 启动检测失败: {e:?}");
                }
            }
            _ => tracing::debug!("startup_action 非 Monitor，不派发启动动作"),
        }

        // 6. 启动 Axum serve（内部把实际监听端口写入 config/.runtime_port；
        //    回环绑定 + Host 头校验策略由 HostPolicy::from_bind_ip 自动推导）
        let axum = app::start_axum_with_listener(container.clone(), log_tx, prepared, None, false)
            .map_err(|e| anyhow::anyhow!("Web 控制台启动失败: {e}"))?;
        tracing::info!(port, "Web 控制台已启动: http://127.0.0.1:{port}");
        Ok(AndroidInstance {
            base_path: base_path.to_path_buf(),
            port,
            shutdown_token: shutdown_token.clone(),
            startup: Some(startup),
            axum: Some(axum),
            container: Some(container),
        })
    }
    .await;
    if started.is_err() {
        shutdown_token.cancel();
    }
    started
}

/// 逆序关闭已启动的服务（对齐 `launcher::shutdown::graceful_shutdown`，
/// 去除托盘 / 实例锁 / 更新 helper 三项桌面侧关注点）
///
/// 注意：本函数**不得**释放日志 guard（`flush_stashed_log_guard`）——tracing
/// subscriber 跨实例沿用，关闭写线程会让重试 / 重启实例的文件日志永久静默；
/// guard 随进程退出统一回收。
async fn shutdown_android(instance: &mut AndroidInstance) {
    // 0. 先取消应用级关闭令牌：容器内 uptime / 登录 shutdown child 自动传播，
    //    在途登录 task（detached，Engine 不持有句柄）协作退出，避免在 Bridge
    //    关闭后仍引用已回收的 Worker
    instance.shutdown_token.cancel();

    // 1. Scheduler（5s 预算，对齐 launcher）
    if let Some(startup) = instance.startup.take() {
        let StartupHandles {
            scheduler_handle,
            bridge_handle,
            event_sink,
            notification,
        } = startup;
        scheduler_handle
            .stop_with_timeout(Duration::from_secs(5))
            .await;

        // 2. Engine（先于 Bridge：Engine 可能正在使用 Bridge；经 EngineSlot 派发到
        //    「当前活跃」Engine 并等待其 run_loop 完全退出，5s 超时兜底）
        if let Some(container) = instance.container.take() {
            let completed = container
                .engine
                .current_handle()
                .map(|h| h.completed.clone());
            let _ = container.engine.dispatch(EngineCommand::Shutdown).await;
            if let Some(token) = completed
                && tokio::time::timeout(Duration::from_secs(5), token.cancelled())
                    .await
                    .is_err()
            {
                tracing::warn!("Engine 关闭超时，继续关闭 Bridge");
            }
            // 6. 清理运行端口文件（对齐 launcher 第 6 步；先取 base_path 再 drop 容器）
            let base_path = container.config.base_path();
            let _ = std::fs::remove_file(crate::utils::paths::runtime_port_path(&base_path));
            drop(container);
        }

        // 3. Bridge（8s 预算：停止 supervisor 主循环并回收 Worker 与残留子进程）
        bridge_handle
            .stop_with_timeout(Duration::from_secs(8))
            .await;

        // 4. 推送通知 + EventSink（两者均受关闭令牌驱动会自行排空退出，此处显式
        //    stop 兜底回收 task——EventSink 句柄本就保留供接入方显式停用）
        notification.stop_with_timeout(Duration::from_secs(3)).await;
        event_sink.stop_with_timeout(Duration::from_secs(3)).await;
    }

    // 5. Axum（内部含 5s 超时与 abort 兜底，历史遗留 #18）
    if let Some(axum) = instance.axum.take() {
        app::stop_axum(axum).await;
    }
}
