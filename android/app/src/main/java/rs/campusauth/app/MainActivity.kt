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
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * 主界面：系统 WebView 加载本机 axum 服务，展示既有 Vue 前端。
 *
 * 启动顺序（P0）：请求通知运行时权限 → 拉起前台服务 → [AuthNative.nativeInit]
 * 拿监听端口 → 后台线程同步 GET `/api/auth/local-token` 取 token → 带 `#auth=`
 * fragment 打开界面。
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

        // 3) 初始化 Rust 核心，拿到 axum 监听端口
        //    P0 在主线程同步调用：预期核心启动耗时毫秒级；若日后变重需移后台线程
        val port = AuthNative.nativeInit(filesDir.absolutePath)
        if (port <= 0) {
            showCoreError()
            return
        }
        // 核心就绪后服务才开始轮询状态，避免对未初始化的 native 层空转
        AuthForegroundService.markNativeReady()

        // 4) 后台线程取 token，取到后回 UI 线程加载界面
        fetchTokenAndLoad(port)
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

    /** 后台线程同步取 token（兼作服务就绪探针），回 UI 线程加载界面 */
    private fun fetchTokenAndLoad(port: Int) {
        thread(start = true, name = "local-token-fetch") {
            val token = fetchLocalToken(port)
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                // TODO(P1) 契约澄清：`#auth=` 的既有语义是 bootstrap 秘钥（64hex，经
                //  /api/auth/exchange 换取），会话 token 放进去会被前端误判为秘钥并
                //  先尝试一次必败的 exchange（401），随后自动回落 local-token 免引导
                //  兜底——行为正确但多一次往返。与前端定稿 token 注入的正式通道后调整。
                val fragment = if (token != null) "#auth=$token" else ""
                webView.loadUrl("http://127.0.0.1:$port/$fragment")
            }
        }
    }

    /** 同步 GET /api/auth/local-token（响应 `{"data":{"token":"..."}}`）；失败返回 null */
    private fun fetchLocalToken(port: Int): String? {
        var connection: HttpURLConnection? = null
        return try {
            connection =
                URL("http://127.0.0.1:$port/api/auth/local-token").openConnection() as HttpURLConnection
            connection.connectTimeout = 5000
            connection.readTimeout = 5000
            connection.requestMethod = "GET"
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            JSONObject(body).optJSONObject("data")?.optString("token")?.takeIf { it.isNotEmpty() }
        } catch (_: Exception) {
            // 服务未就绪 / 响应异常：不阻塞界面加载，前端回环下会自行取用 token
            null
        } finally {
            connection?.disconnect()
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
