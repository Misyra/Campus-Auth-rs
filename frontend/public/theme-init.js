/* 首屏主题预置：在首帧前恢复持久化的主题，避免浅色用户闪一下深色（FOUC）。
   外置为独立文件以满足 CSP script-src 'self'（内联脚本会被浏览器拦截）。
   仅设置 data-theme、自定义背景色与壁纸变量，其余变量交给 base.css 的静态默认值；
   Vue 挂载后 useAppearance.applyAppearance 会全量接管。 */
(function () {
  try {
    var a = JSON.parse(localStorage.getItem("appearance") || "{}");
    var dark =
      a.theme === "auto"
        ? matchMedia("(prefers-color-scheme: dark)").matches
        : (a.theme || "light") === "dark";
    var root = document.documentElement;
    root.setAttribute("data-theme", dark ? "dark" : "light");
    if (a.background_color) {
      root.style.setProperty("--bg-primary", a.background_color);
    }
    // 自定义壁纸同源预置：body 此刻尚不存在（has-custom-bg class 要等 Vue 挂载），
    // 大图用户原来每次加载都先闪一帧纯底色；改挂在 html 属性 + 变量上，首帧即成像。
    // 变量名/转义与 applyAppearance 保持一致（url() 用 JSON.stringify 引注）。
    if (a.background_url) {
      root.setAttribute("data-custom-bg", "");
      root.style.setProperty("--bg-image", "url(" + JSON.stringify(String(a.background_url)) + ")");
      root.style.setProperty("--bg-blur", "blur(" + (Number(a.background_blur) || 0) + "px)");
      root.style.setProperty("--bg-opacity", String(typeof a.background_opacity === "number" ? a.background_opacity : 0.3));
    }
  } catch (e) { /* localStorage 不可用或数据损坏时放弃预置，保持 base.css 默认主题 */ }
})();
