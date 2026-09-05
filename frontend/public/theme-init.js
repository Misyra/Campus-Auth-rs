/* 首屏主题预置：在首帧前恢复持久化的主题，避免浅色用户闪一下深色（FOUC）。
   外置为独立文件以满足 CSP script-src 'self'（内联脚本会被浏览器拦截）。
   仅设置 data-theme 与自定义背景色，其余变量交给 base.css 的静态默认值；
   Vue 挂载后 useAppearance.applyAppearance 会全量接管。 */
(function () {
  try {
    var a = JSON.parse(localStorage.getItem("appearance") || "{}");
    var dark =
      a.theme === "auto"
        ? matchMedia("(prefers-color-scheme: dark)").matches
        : (a.theme || "light") === "dark";
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
    if (a.background_color) {
      document.documentElement.style.setProperty("--bg-primary", a.background_color);
    }
  } catch (e) {}
})();
