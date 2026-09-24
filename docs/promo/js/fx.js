/* ============================================================
   分页小特效
   1) runCounters —— 数字滚动（[data-count]）
   2) runScramble —— AES 密文先乱码再落定（#cipher）
   3) runStagger  —— 把一组子元素的 --d 按顺序铺开（列表用它做逐条入场）
   都在换页时被 deck.js 调用；元素离开当前页就自行停止，不会串台。
   ============================================================ */

window.PROMO = window.PROMO || {};

(function (P) {
  'use strict';

  const HEX = '0123456789abcdefABCDEF';
  const reduce = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  /* ---------- 数字滚动 ---------- */
  function runCounters(slide) {
    slide.querySelectorAll('[data-count]').forEach(el => {
      const to = parseFloat(el.dataset.count);
      if (!Number.isFinite(to)) return;
      if (reduce()) { el.textContent = String(to); return; }
      const dur = 950, t0 = performance.now();
      (function step(now) {
        const k = Math.min((now - t0) / dur, 1);
        el.textContent = String(Math.round(to * (1 - Math.pow(1 - k, 3))));
        if (k < 1 && slide.classList.contains('is-active')) requestAnimationFrame(step);
      })(t0);
    });
  }

  /* ---------- 密文乱码 ---------- */
  function runScramble(slide) {
    const el = slide.querySelector('#cipher');
    if (!el) return;
    const target = el.dataset.text || el.textContent;
    el.dataset.text = target;
    if (reduce()) { el.textContent = target; return; }
    let n = 0;
    const iv = setInterval(() => {
      if (++n > 16 || !slide.classList.contains('is-active')) {
        clearInterval(iv);
        el.textContent = target;
        return;
      }
      let s = 'v1:';
      for (let i = 0; i < 7; i++) s += HEX[Math.floor(Math.random() * 16)];
      el.textContent = s + '…';
    }, 45);
  }

  /* ---------- 逐条入场：给容器里每个 [data-stagger] 子元素排延迟 ---------- */
  function runStagger(slide) {
    slide.querySelectorAll('[data-stagger]').forEach(box => {
      const step = Number(box.dataset.stagger) || 60;
      Array.from(box.children).forEach((child, i) => {
        if (!child.style.getPropertyValue('--d')) {
          child.style.setProperty('--d', (step * i) + 'ms');
        }
      });
    });
  }

  P.fx = {
    enter: function (slide) {
      runCounters(slide);
      runScramble(slide);
      runStagger(slide);
    }
  };
})(window.PROMO);
