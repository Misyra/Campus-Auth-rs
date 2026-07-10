import { createApp } from "vue";
import App from "./App.vue";
import { router } from "./router";
import { useUi } from "./composables/useUi";
import "./styles/index.css";

const app = createApp(App);
app.use(router);
app.mount("#app");

// 应用挂载后触发初始化：拉取配置/状态/日志、建立 WebSocket、检查更新。
void useUi().init();
