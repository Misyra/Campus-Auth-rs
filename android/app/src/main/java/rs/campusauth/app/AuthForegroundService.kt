package rs.campusauth.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import org.json.JSONObject

/**
 * 认证守护前台服务：Rust 认证核心的常驻生命周期载体。
 *
 * 服务与 UI 解耦：MainActivity 只负责初始化核心与展示界面，服务独立存活
 * （START_STICKY，被系统回收后尽量重建）。P0 用 5s 粗轮询 [AuthNative.nativeStatus]
 * 刷新通知文案，TODO(P1) 改为 WS 订阅状态推送；通知点击回到 MainActivity。
 */
class AuthForegroundService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private var polling = false

    private val pollRunnable = object : Runnable {
        override fun run() {
            refreshNotification()
            handler.postDelayed(this, POLL_INTERVAL_MS)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // targetSdk 34+ 要求启动后尽快进入前台，类型必须与清单声明的 specialUse 一致
        startForegroundWithType()
        if (!polling) {
            polling = true
            handler.post(pollRunnable)
        }
        // START_STICKY：被系统杀死后尽量重建（重建时 intent 为空，不影响本服务流程）
        return START_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(pollRunnable)
        try {
            // 核心可能尚未初始化（如开机自启后从未打开过 App），Rust 侧须容忍该路径
            AuthNative.nativeStop()
        } catch (_: Throwable) {
            // 停止核心失败不应阻断服务销毁（含 .so 缺失时的 UnsatisfiedLinkError）
        }
        AuthForegroundService.markNativeStopped()
        super.onDestroy()
    }

    /** 进入前台：specialUse 类型；首次展示「初始化中」占位文案 */
    private fun startForegroundWithType() {
        val notification = buildNotification(getString(R.string.notif_text_waiting))
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            notification,
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
        )
    }

    /** 创建静默常驻通知渠道（IMPORTANCE_LOW，状态刷新不发声） */
    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.channel_name),
            NotificationManager.IMPORTANCE_LOW,
        )
        channel.description = getString(R.string.channel_description)
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /** 每 5s 轮询核心状态并更新通知文案 */
    private fun refreshNotification() {
        if (!AuthForegroundService.isNativeReady()) {
            // 核心未就绪：不触碰 native 层，仅提示用户打开应用完成初始化
            postNotification(getString(R.string.notif_text_waiting))
            return
        }
        val statusJson = try {
            AuthNative.nativeStatus()
        } catch (_: Throwable) {
            // native 异常不应拖垮前台服务本身
            postNotification(getString(R.string.notif_text_core_error))
            return
        }
        postNotification(statusSummary(statusJson))
    }

    /** 与 startForeground 同 id：直接更新常驻通知（IMPORTANCE_LOW 渠道静默） */
    private fun postNotification(text: String) {
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun buildNotification(text: String): Notification {
        // 点击通知回到主界面（FLAG_IMMUTABLE 为 targetSdk 31+ 强制口径）
        val openIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            // TODO(P1) 换正式应用图标：临时借用平台同步图标占位
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(text)
            .setContentIntent(openIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .build()
    }

    /**
     * 状态摘要：nativeStatus 契约只约定 JSON、字段未定稿，P0 尽力取 `status`
     * 字段渲染，取不到就原样截断展示；TODO(P1) 按状态字典渲染 + WS 订阅
     */
    private fun statusSummary(json: String): String {
        val summary = try {
            val status = JSONObject(json).optString("status")
            if (status.isNotEmpty()) status else json
        } catch (_: Exception) {
            json
        }
        return if (summary.length > MAX_STATUS_TEXT_LEN) {
            summary.take(MAX_STATUS_TEXT_LEN) + "…"
        } else {
            summary
        }
    }

    companion object {
        /** 通知渠道 id */
        private const val CHANNEL_ID = "auth_guard"

        /** 常驻通知 id（startForeground 与 notify 共用） */
        private const val NOTIFICATION_ID = 1

        /** P0 粗轮询间隔；TODO(P1) 换 WS 订阅状态推送 */
        private const val POLL_INTERVAL_MS = 5_000L

        /** 通知文案最大长度（状态 JSON 原样展示时防溢出） */
        private const val MAX_STATUS_TEXT_LEN = 120

        @Volatile
        private var nativeReady = false

        /** MainActivity 完成 nativeInit 后调用：此后服务才开始轮询核心状态 */
        fun markNativeReady() {
            nativeReady = true
        }

        /** 服务销毁时复位：核心重新初始化前不触碰 native 层 */
        fun markNativeStopped() {
            nativeReady = false
        }

        /** 核心是否已初始化（服务轮询守卫） */
        fun isNativeReady(): Boolean = nativeReady
    }
}
