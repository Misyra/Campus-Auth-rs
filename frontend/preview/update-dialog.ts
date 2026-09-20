/**
 * 更新弹窗的样式预览入口（本地开发用，不参与打包）。
 *
 * 直接给 `useUpdateDialog` 塞假数据把弹窗立起来，**不依赖后端**：想看样式时不必启动
 * 主程序，也不必真的存在更高版本。右上角浮动切换器可切换各种状态。
 *
 * 正文用的是线上 v5.0.2 发布说明原文（与 `src/utils/releaseNotes.test.ts` 的 fixture 同源），
 * 便于直接判断真实正文的排版观感。
 */
import { createApp, h, reactive } from "vue";

import UpdateDialog from "@/components/UpdateDialog.vue";
import { useUpdateDialog } from "@/composables/useUpdateDialog";
import "@/styles/index.css";

/** 线上 `releases/latest` 的 v5.0.2 body 原文 */
const REAL_NOTES = [
  "## 更新日志",
  "",
  "## v5.0.2（2026-09-20）",
  "",
  "### 修复",
  "",
  "- **应用内更新下载完成后无法生效**：此前点「立即更新」下载完成后，若在弹窗中选择「立即重启」，程序会用旧版本生成一个替代进程再退出——替代进程恰好锁住主程序文件，导致更新助手替换失败（日志出现「替换失败: 另一个程序正在使用此文件」），重启后仍是旧版本，且已下载的更新包被清理、需重新下载。现在带待应用更新重启时不再生成替代进程，由更新助手完成替换后直接用新版本启动。手动选择安装包更新与定时自动重启场景一并修复；即便助手替换意外失败，也会保留已下载的更新包，下次启动自动重试应用，无需重新下载。",
  "",
  "## 平台运行说明",
  "",
  "- **Windows**：解压后直接运行 campus-auth.exe（GUI 程序，无控制台窗口，自动打开浏览器 Web 控制台）",
  "- **macOS**：浏览器下载的二进制带 quarantine 属性，Gatekeeper 会拦截，首次运行前先执行 `xattr -cr campus-auth`（或对解压后的整个目录 `xattr -cr .`）；解压用 `tar -xzf`",
  "- **Linux**：二进制动态链接 GTK3 / libayatana-appindicator / librsvg（托盘），最小系统需先安装运行时库，Debian/Ubuntu：`sudo apt install libgtk-3-0 libayatana-appindicator3-1 librsvg2-2`；解压用 `tar -xzf`，执行 `./campus-auth`",
  "- **Linux ARM64**：同上依赖；构建于 ubuntu-24.04（glibc 较新），老发行版（Debian 11/Ubuntu 20.04 及更早）可能因 glibc 过旧无法运行，建议 Debian 12+/Ubuntu 22.04+；树莓派 OS（64 位）可直接运行",
  "- **Docker**：预构建多架构镜像为 `ghcr.io/misyra/campus-auth-rs:v5.0.2`，`prerelease` 指向最新测试版，正式版更新 `latest`",
  "- 各平台 .sha256 为对应压缩包的 SHA256 校验文件",
  "",
].join("\n");

const update = useUpdateDialog();

const BASE = {
  latest: "5.0.3",
  current: "5.0.2",
  notes: REAL_NOTES,
  release_date: "2026-09-20T17:56:03Z",
  size: 12902345,
  url: "https://github.com/Misyra/Campus-Auth-rs/releases/download/v5.0.3/campus-auth-v5.0.3-x86_64-pc-windows-msvc.zip",
  sha256: "0".repeat(64),
};

/** 预览状态：`?state=` 指定，点右上角切换器也能换（不重新加载页面） */
const STATES: Record<string, { label: string; apply: () => void }> = {
  update: {
    label: "发现新版本",
    apply: () => {
      update.state.info = { has_update: true, ...BASE };
      update.state.error = "";
    },
  },
  local: {
    label: "命中本地包",
    apply: () => {
      update.state.info = {
        has_update: true,
        ...BASE,
        local_package: { file_name: "campus-auth-v5.0.3-x86_64-pc-windows-msvc.zip", size: 12902345, sha256: "0".repeat(64) },
      };
      update.state.error = "";
    },
  },
  latest: {
    label: "已是最新（仍看得到当期说明）",
    apply: () => {
      update.state.info = { has_update: false, latest: "5.0.2", current: "5.0.2", notes: REAL_NOTES, release_date: BASE.release_date };
      update.state.error = "";
    },
  },
  nonotes: {
    label: "没有更新说明",
    apply: () => {
      update.state.info = { has_update: true, ...BASE, notes: "" };
      update.state.error = "";
    },
  },
  ready: {
    label: "更新已就绪",
    apply: () => {
      update.state.info = { has_update: false, message: "更新已就绪，重启后生效" };
      update.state.error = "";
    },
  },
  error: {
    label: "检查失败",
    apply: () => {
      update.state.info = null;
      update.state.error = "请求失败 (503)";
    },
  },
  loading: {
    label: "检查中",
    apply: () => {
      update.state.info = null;
      update.state.error = "";
      update.state.loading = true;
    },
  },
};

const active = reactive({ key: "" });

function select(key: string): void {
  active.key = key;
  update.state.loading = false;
  STATES[key].apply();
  update.state.open = true;
}

const switcher = {
  setup() {
    return () =>
      h(
        "div",
        {
          style:
            "position:fixed;top:12px;right:12px;z-index:99999;display:flex;flex-wrap:wrap;gap:6px;max-width:420px;justify-content:flex-end",
        },
        Object.entries(STATES).map(([key, state]) =>
          h(
            "button",
            {
              type: "button",
              onClick: () => select(key),
              style: `padding:4px 10px;border-radius:999px;border:1px solid rgba(148,163,184,.4);cursor:pointer;font-size:12px;${
                active.key === key
                  ? "background:#e5e7eb;color:#111"
                  : "background:rgba(15,23,42,.75);color:#e5e7eb"
              }`,
            },
            state.label,
          ),
        ),
      );
  },
};

const requested = new URLSearchParams(location.search).get("state") ?? "update";
select(requested in STATES ? requested : "update");

// `?theme=dark|light` 直覆盖首屏主题（theme-init.js 已设过一次，这里按预览参数改写），
// 便于并排比较两套主题下的观感
const requestedTheme = new URLSearchParams(location.search).get("theme");
if (requestedTheme === "dark" || requestedTheme === "light") {
  document.documentElement.setAttribute("data-theme", requestedTheme);
}

createApp({ render: () => [h(UpdateDialog), h(switcher)] }).mount("#preview");
