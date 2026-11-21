package rs.campusauth.app

/**
 * Rust 认证核心（libcampus_auth.so）的 JNI 绑定。
 *
 * 契约与 Rust 侧约定死，不得改名。补充实现侧须知：
 * - 这些是 Kotlin `object` 的实例方法（非 `@JvmStatic`），JNI 符号按实例方法约定：
 *   `Java_rs_campusauth_app_AuthNative_<方法名>(JNIEnv*, jobject, ...)`；
 * - [nativeInit] 必须幂等：核心已运行时直接返回现有端口，不得重复起服务
 *   （Activity 重建 / 进程恢复会再次调用）；停止后再次调用会重新引导新实例
 *   （生命周期可重启，Rust 侧先等上一轮关闭完成）；
 * - 其余方法必须容忍「未初始化」状态（nativePort 返回 0、nativeStatus 返回占位
 *   JSON）——开机自启会先拉起前台服务轮询状态，[nativeInit] 要等用户打开 App
 *   才执行。
 */
object AuthNative {

    init {
        // Rust 产物库名固定为 libcampus_auth.so，经 app/src/main/jniLibs 打进 APK
        System.loadLibrary("campus_auth")
    }

    /** 初始化核心并以 baseDir 为根落盘运行时目录，返回 axum 监听端口；0 = 失败 */
    external fun nativeInit(baseDir: String): Int

    /** 当前 axum 监听端口；未初始化时返回 0 */
    external fun nativePort(): Int

    /** 核心状态快照，JSON 字符串（字段契约 TODO(P1) 与 Rust 侧定稿） */
    external fun nativeStatus(): String

    /**
     * 停止核心。**异步受理**：转交 Rust 后台执行全链优雅关闭后立即返回，
     * 调用方（如前台服务 onDestroy 的主线程）不得假设关闭已完成；
     * 非 0 = 已受理（含本就未初始化/已停止的幂等情形），0 = 受理失败
     * （如核心初始化进行中）。
     */
    external fun nativeStop(): Int
}
