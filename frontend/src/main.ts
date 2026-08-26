import { createApp } from "vue";
import App from "./App.vue";
import { router } from "./router";
import { useUi } from "./composables/useUi";
import { frontendLogger } from "./utils/logger";
import "./styles/index.css";

const app = createApp(App);

// 全局错误兜底：嵌入场景用户不会打开 devtools，未捕获异常至少要进入
// 前端日志上报（scope "frontend"），否则只会落 console、无从排查。
// 注意：frontendLogger 内部已 try/catch 上报通道，此处不再向外抛出，
// 避免日志上报自身异常再次触发 errorHandler 造成循环。
app.config.errorHandler = (err, instance, info) => {
  let component = "";
  try {
    const name = (instance as { type?: { name?: string } } | null)?.type?.name;
    if (name) component = `，组件 <${name}>`;
  } catch {
    /* 组件信息提取失败不影响上报本身 */
  }
  frontendLogger.error("frontend", `未捕获的组件异常（${info}${component}）`, err);
};

window.addEventListener("unhandledrejection", (ev) => {
  // 前端大量异步回调无 await 落点，被拒 Promise 在此统一记录
  frontendLogger.error("frontend", "未处理的 Promise 拒绝", ev.reason);
});

app.use(router);
app.mount("#app");

// 应用挂载后触发初始化：拉取配置/状态/日志、建立 WebSocket、检查更新。
void useUi().init();
