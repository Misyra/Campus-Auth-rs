package rs.campusauth.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.DownloadManager
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import android.util.Base64
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
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
        // P2-10：不设 WebChromeClient 时 `input.click()` 静默 no-op——导入方案/
        // 任务/更新包、选 OCR 截图与背景图全部失效，必须接系统文件选择器
        webView.webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(
                view: WebView,
                callback: ValueCallback<Array<Uri>>,
                params: FileChooserParams,
            ): Boolean {
                openFileChooser(callback, params)
                return true
            }
        }
        // P2-10：前端导出（日志/诊断包/方案）经 blob: URL 触发 `<a download>`，
        // 无 DownloadListener 时下载完全不生效
        webView.setDownloadListener { url, _, mimetype, _, _ ->
            handleDownload(url, mimetype)
        }
    }

    // ============ 文件选择（P2-10，onShowFileChooser → 系统选择器） ============

    /** 挂起的文件选择回调：新请求到来时旧回调必须先以 null 结算（WebView 契约：恰好调用一次） */
    private var fileChooserCallback: ValueCallback<Array<Uri>>? = null

    /**
     * 前端 `input.click()` 的文件选择交系统 ACTION_GET_CONTENT
     *
     * acceptTypes 为前端 input accept 按逗号拆分的结果（如 [".zip",".tar.gz"]
     * 或 ["application/json",".json"]）：ACTION_GET_CONTENT 的 type 只认单条
     * MIME，取首个含 "/" 的条目，纯扩展名列表退 "*/*" 由系统选择器过滤兜底。
     * 结果经 onActivityResult 回传（取消时 parseResult 返回 null 恰好结算回调）。
     */
    private fun openFileChooser(
        callback: ValueCallback<Array<Uri>>,
        params: WebChromeClient.FileChooserParams,
    ) {
        // 前一个未结算的回调先取消，否则 WebView 的选择流程永久挂起
        fileChooserCallback?.onReceiveValue(null)
        fileChooserCallback = callback
        val mime = params.acceptTypes.firstOrNull { it.contains('/') }?.trim()
            ?.takeIf { it.isNotEmpty() } ?: "*/*"
        val intent = Intent(Intent.ACTION_GET_CONTENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mime
            if (params.mode == WebChromeClient.FileChooserParams.MODE_OPEN_MULTIPLE) {
                putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
            }
        }
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(intent, REQUEST_CODE_FILE_CHOOSER)
        } catch (_: ActivityNotFoundException) {
            // 无文档选择器可用：立即结算取消，前端 pickFile 得到 null
            fileChooserCallback = null
            callback.onReceiveValue(null)
        }
    }

    @Suppress("DEPRECATION")
    // onActivityResult 自 API 29 起被 ActivityResult API 取代，但仍走经典管线
    // （与 onBackPressed 同口径；单 Activity 壳无复用压力）
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_CODE_FILE_CHOOSER) return
        val callback = fileChooserCallback ?: return
        fileChooserCallback = null
        // parseResult 统一处理单选 data 与多选 clipData；取消时返回 null
        callback.onReceiveValue(
            WebChromeClient.FileChooserParams.parseResult(resultCode, data),
        )
    }

    // ============ 下载（P2-10，DownloadListener → 系统处理） ============

    /**
     * WebView 下载请求交系统：http(s) 直链走 DownloadManager（系统下载目录、
     * 自带进度通知，应用零权限）；blob: URL 系统组件无法直接消费，在 WebView
     * 内 fetch 回读后自行落盘
     */
    private fun handleDownload(url: String, mimetype: String?) {
        if (url.startsWith("blob:")) {
            saveBlobDownload(url, mimetype)
            return
        }
        try {
            val request = DownloadManager.Request(Uri.parse(url))
                .setMimeType(mimetype?.takeIf { it.isNotBlank() } ?: "*/*")
                .setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED)
            getSystemService(DownloadManager::class.java).enqueue(request)
            Toast.makeText(this, R.string.download_queued, Toast.LENGTH_SHORT).show()
        } catch (_: Exception) {
            // DownloadManager 不可用 / URL 非法：交系统浏览器兜底（ACTION_VIEW）
            try {
                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
            } catch (_: ActivityNotFoundException) {
                Toast.makeText(this, R.string.download_failed, Toast.LENGTH_SHORT).show()
            }
        }
    }

    /**
     * blob: URL 下载：在 WebView 内 fetch 转 data URL，主线程回调里解码落盘
     *
     * blob: URL 只在本 WebView 进程内有意义（DownloadManager/浏览器均无法
     * 消费），用一次性脚本 fetch 回读；evaluateJavascript 的结果以 JSON 字符串
     * 形态回传，data URL 仅含 base64 字母表无转义字符，去引号即可还原
     */
    private fun saveBlobDownload(url: String, mimetype: String?) {
        val script = "(async()=>{try{" +
            "const b=await fetch('$url').then(r=>r.blob());" +
            "return await new Promise(res=>{const fr=new FileReader();" +
            "fr.onload=()=>res(fr.result);fr.onerror=()=>res(null);" +
            "fr.readAsDataURL(b);});" +
            "}catch(e){return null}})()"
        webView.evaluateJavascript(script) { result ->
            val dataUrl = result?.takeIf { it != "null" }?.removeSurrounding("\"")
            if (dataUrl == null) {
                Toast.makeText(this, R.string.download_failed, Toast.LENGTH_SHORT).show()
                return@evaluateJavascript
            }
            writeDataUrlToDownloads(dataUrl, mimetype)
        }
    }

    /** data URL 解码后写入系统下载集合（API 29+）/应用外部下载目录（旧版本回退） */
    private fun writeDataUrlToDownloads(dataUrl: String, mimetype: String?) {
        val comma = dataUrl.indexOf(',')
        if (!dataUrl.startsWith("data:") || comma < 0) {
            Toast.makeText(this, R.string.download_failed, Toast.LENGTH_SHORT).show()
            return
        }
        val bytes = try {
            Base64.decode(dataUrl.substring(comma + 1), Base64.DEFAULT)
        } catch (_: IllegalArgumentException) {
            Toast.makeText(this, R.string.download_failed, Toast.LENGTH_SHORT).show()
            return
        }
        // data: 头内嵌的 MIME（"image/png;base64" 形态）优先于 DownloadListener
        // 报告的 mimetype（blob 场景后者常为空）
        val header = dataUrl.substring(5, comma).substringBefore(';')
        val mime = mimetype?.takeIf { it.isNotBlank() }
            ?: header.takeIf { it.isNotEmpty() && it != "base64" }
            ?: "application/octet-stream"
        // 前端 `<a download>` 的文件名不随 DownloadListener 传递（取不到原始名），
        // 以 MIME 推导扩展名 + 时间戳命名，保证落盘文件可辨识
        val fileName = "campus-auth-${exportTimestamp()}.${extensionOf(mime)}"
        val location: String?
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            location = saveViaMediaStore(fileName, mime, bytes)
        } else {
            location = saveToAppDownloads(fileName, bytes)
        }
        val text = location?.let { getString(R.string.download_saved, it) }
        Toast.makeText(this, text ?: getString(R.string.download_failed), Toast.LENGTH_LONG).show()
    }

    /** API 29+：MediaStore.Downloads 插入（无需权限，系统「下载」目录对用户可见） */
    private fun saveViaMediaStore(fileName: String, mime: String, bytes: ByteArray): String? =
        try {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, fileName)
                put(MediaStore.Downloads.MIME_TYPE, mime)
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
            }
            val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: return null
            contentResolver.openOutputStream(uri)?.use { it.write(bytes) } ?: return null
            "Download/$fileName"
        } catch (_: Exception) {
            null
        }

    /** API 26-28 兼容回退：应用外部下载目录（无需权限；无 MediaStore.Downloads 集合） */
    private fun saveToAppDownloads(fileName: String, bytes: ByteArray): String? =
        try {
            // 无外部存储（如手表/车机形态）时退应用私有目录
            val dir = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: filesDir
            val file = File(dir, fileName)
            file.outputStream().use { it.write(bytes) }
            file.absolutePath
        } catch (_: Exception) {
            null
        }

    /** 导出文件名时间戳（本地时区）：yyyyMMdd_HHmmss */
    private fun exportTimestamp(): String =
        SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(Date())

    /** 常见导出 MIME 的扩展名；未识别回退 bin */
    private fun extensionOf(mime: String): String = when (mime) {
        "application/zip" -> "zip"
        "application/json" -> "json"
        "text/plain" -> "txt"
        "image/png" -> "png"
        "image/jpeg" -> "jpg"
        else -> "bin"
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

        /** 文件选择请求码（P2-10，onShowFileChooser → 系统选择器） */
        private const val REQUEST_CODE_FILE_CHOOSER = 2
    }
}
