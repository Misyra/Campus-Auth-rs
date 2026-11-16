package rs.campusauth.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

/**
 * 开机自启广播接收器：BOOT_COMPLETED 后按用户设置决定是否拉起前台服务。
 *
 * 是否自启由 SharedPreferences 的 `autostart_enabled` 控制（默认 false，
 * TODO(P2) 设置 UI）。BOOT_COMPLETED 属于系统 FGS 启动豁免清单，允许从此处
 * 拉起前台服务。
 */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return

        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        if (!prefs.getBoolean(KEY_AUTOSTART_ENABLED, false)) return

        ContextCompat.startForegroundService(
            context,
            Intent(context, AuthForegroundService::class.java),
        )
    }

    companion object {
        /** SharedPreferences 文件名（与 TODO(P2) 的设置 UI 共用） */
        const val PREFS_NAME = "app_settings"

        /** 是否开机自启，默认 false */
        const val KEY_AUTOSTART_ENABLED = "autostart_enabled"
    }
}
