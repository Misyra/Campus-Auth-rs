package rs.campusauth.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import kotlin.concurrent.thread

/**
 * 主界面：系统 WebView 加载本机 axum 服务，展示既有 Vue 前端。
 *
 * 启动顺序：请求通知运行时权限 → 拉起前台服务 → 后台线程 [AuthNative.nativeInit]
 * 拿监听端口（前置②：Rust 契约明示数秒量级，主线程同步调用会 ANR 并推迟
 * startForeground 出系统窗口）→ 经 JNI 读取 bootstrap 秘钥 → 回主线程带
 * `#auth=` fragment 打开界面（P2-11：安卓回环为全设备应用共享，local-token
 * 免引导端点已在 Rust 侧关闭，引导改走前端既有 exchange 握手通道）。
 *
 * 本界面不管理核心生命周期：前台服务独立于 UI 存活，onDestroy 只销毁 WebView、
 * 不停服务。
 */
class MainActivity : Activity() {

    private lateinit var webView: WebView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        webView = WebView(this)
        setContentView(webView)
        setupWebView()
        // targetSdk 35（Android 15）强制 edge-to-edge：手动让出系统栏内边距
        ViewCompat.setOnApplyWindowInsetsListener(webView) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }

        // 1) 通知运行时权限（API 33+；被拒绝只影响通知可见性，不影响服务与认证）
        maybeRequestNotificationPermission()

        // 2) 先拉起前台服务：服务独立于本界面存活，先于核心初始化就位
        ContextCompat.startForegroundService(this, Intent(this, AuthForegroundService::class.java))

        // 3) 初始化 Rust 核心：后台线程执行（前置②），完成后回 UI 线程经
        //    #auth fragment 加载界面
        initCoreAndLoad()
    }

    /** WebView 基础配置：仅 JS + DOM 存储；不加载 file:// 页面，不开文件访问 */
    @SuppressLint("SetJavaScriptEnabled")
    private fun setupWebView() {
        with(webView.settings) {
            javaScriptEnabled = true
            domStorageEnabled = true
            // 前端为响应式 SPA：按设备宽度渲染
            useWideViewPort = true
            loadWithOverviewMode = true
        }
        webView.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(
                view: WebView,
                request: WebResourceRequest,
            ): Boolean {
                val url = request.url
                // 仅本机 axum 服务留在 WebView，其余链接交给系统浏览器
                if (url.host == "127.0.0.1") return false
                return try {
                    startActivity(Intent(Intent.ACTION_VIEW, url))
                    true
                } catch (_: ActivityNotFoundException) {
                    // 设备无浏览器可接管外链：留在 WebView 也不安全，直接吞掉
                    true
                }
            }
        }
    }

    /**
     * 后台线程初始化核心，完成后回主线程经 `#auth=<bootstrap>` fragment 加载界面
     *
     * 前置②：nativeInit 为数秒量级的同步调用，必须离开主线程；失败时同样回
     * 主线程展示错误页（不更新 UI 的后台线程直接 return）
     */
    private fun initCoreAndLoad() {
        thread(start = true, name = "core-init") {
            val port = try {
                AuthNative.nativeInit(filesDir.absolutePath)
            } catch (_: Throwable) {
                // .so 缺失（UnsatisfiedLinkError）等 JNI 层异常按启动失败处理
                0
            }
            if (port <= 0) {
                runOnUiThread { if (!isFinishing && !isDestroyed) showCoreError() }
                return@thread
            }
            // 核心就绪后前台服务才开始轮询状态，避免对未初始化的 native 层空转；
            // 仅 volatile 写，可安全地在后台线程执行
            AuthForegroundService.markNativeReady()
            // P2-11：安卓构建已关闭 local-token 免引导端点（回环为全设备应用
            // 共享，免引导等于向任意第三方 App 发放 token），界面引导改经 JNI
            // 读取 bootstrap 秘钥拼 #auth fragment——前端既有 exchange 通道零改动
            val secret = try {
                AuthNative.nativeBootstrapSecret()
            } catch (_: Throwable) {
                null
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                // 秘钥读取失败（理论上 init 成功后不会发生）：退化为无 fragment
                // 打开，前端会呈现引导失败态并给出粘贴秘钥的恢复入口
                val fragment = if (!secret.isNullOrEmpty()) "#auth=$secret" else ""
                webView.loadUrl("http://127.0.0.1:$port/$fragment")
            }
        }
    }

    /** 核心启动失败：提示 + 简易错误页，避免白屏；前台服务仍继续运行 */
    private fun showCoreError() {
        Toast.makeText(this, R.string.core_init_failed, Toast.LENGTH_LONG).show()
        val html = "<html><head><meta charset=\"utf-8\"></head><body>" +
            "<h3>${getString(R.string.core_init_failed)}</h3></body></html>"
        webView.loadDataWithBaseURL(null, html, "text/html", "utf-8", null)
    }

    /** API 33+ 请求通知运行时权限；结果不阻塞启动流程 */
    private fun maybeRequestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            ActivityCompat.requestPermissions(
                this,
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                REQUEST_CODE_NOTIFICATIONS,
            )
        }
    }

    @Suppress("DEPRECATION")
    // 未启用 predictive back（enableOnBackInvokedCallback 默认 false），仍走经典返回管线
    override fun onBackPressed() {
        if (webView.canGoBack()) {
            webView.goBack()
        } else {
            super.onBackPressed()
        }
    }

    override fun onDestroy() {
        // 只销毁 WebView，不停前台服务：守护与 UI 生命周期解耦
        webView.destroy()
        super.onDestroy()
    }

    companion object {
        /** 通知运行时权限请求码 */
        private const val REQUEST_CODE_NOTIFICATIONS = 1
    }
}
