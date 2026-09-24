/* ============================================================
   放映引擎
   一次 rAF 循环同时驱动三件事：本页计时 → 进度线 → 背景网格。
   舞台固定 1600×900 等比缩放；全片统一深色主题，背景气候（data-mood）
   只改变纹理密度与构图层次，翻页时交叉过渡。
   经典脚本，file:// 双击即用。
   ============================================================ */

(function () {
  'use strict';

  const stage     = document.getElementById('stage');
  const backdrop  = document.getElementById('backdrop');
  const slides    = Array.from(stage.querySelectorAll('.slide'));
  const fill      = document.getElementById('railFill');
  const dot       = document.getElementById('railDot');
  const ticksWrap = document.getElementById('railTicks');
  const counter   = document.getElementById('counter');
  const btnPrev   = document.getElementById('btnPrev');
  const btnPlay   = document.getElementById('btnPlay');
  const btnNext   = document.getElementById('btnNext');
  const btnFull   = document.getElementById('btnFull');
  const sweep     = document.getElementById('sweep');
  const hint      = document.getElementById('hint');

  const N = slides.length;
  const DUR = slides.map(s => Number(s.dataset.dur) || 7000);
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  let idx = 0;
  let playing = true;         // 默认自动播放，像宣传片
  let finished = false;
  let elapsed = 0;
  let last = 0;
  let raf = 0;
  let interacted = false;
  let scale = 1;

  const pad2 = n => String(n).padStart(2, '0');

  /* ---------- 连接线上的刻度：每一页一段，点刻度即跳页 ---------- */

  const ticks = slides.map((s, k) => {
    const t = document.createElement('button');
    t.type = 'button';
    t.className = 'rail-tick';
    t.style.left = (k / N * 100) + '%';
    t.title = s.dataset.title || ('第 ' + (k + 1) + ' 页');
    t.setAttribute('aria-label', '跳到第 ' + (k + 1) + ' 页：' + (s.dataset.title || ''));
    t.addEventListener('click', e => { e.stopPropagation(); go(k); });
    ticksWrap.appendChild(t);
    return t;
  });

  /* ---------- 播放 / 暂停 / 重播 ---------- */

  const ICON = {
    play:  '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 5 L19 12 L8 19 Z"/></svg>',
    pause: '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M9.5 5 V19 M14.5 5 V19"/></svg>',
    again: '<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 12 a8 8 0 1 1 -2.4 -5.7"/><path d="M20 4 v4.5 h-4.5"/></svg>'
  };

  function syncPlayIcon() {
    const name = finished ? 'again' : (playing ? 'pause' : 'play');
    btnPlay.innerHTML = ICON[name];
    btnPlay.setAttribute('aria-label', finished ? '重新播放' : (playing ? '暂停自动播放' : '继续自动播放'));
  }

  function paint(p) {
    const pct = Math.max(0, Math.min(1, p)) * 100;
    fill.style.width = pct + '%';
    dot.style.left = pct + '%';
    dot.classList.toggle('is-idle', !playing);
  }

  function runSweep() {
    if (reduce) return;
    sweep.classList.remove('run');
    void sweep.offsetWidth;
    sweep.classList.add('run');
  }

  /* ---------- 换页 ---------- */

  function go(i) {
    const next = Math.max(0, Math.min(N - 1, i));
    if (next === idx) { elapsed = 0; last = performance.now(); return; }

    idx = next;
    finished = false;
    elapsed = 0;
    last = performance.now();

    slides.forEach((s, k) => {
      const on = k === idx;
      s.classList.toggle('is-active', on);
      s.setAttribute('aria-hidden', on ? 'false' : 'true');
    });

    const cur = slides[idx];

    // 保留 data-theme 兼容旧分镜；当前宣传片全片统一为深色主题。
    stage.classList.toggle('is-night', cur.dataset.theme === 'night');

    // 背景气候：每页换一层底，翻页时交叉淡入
    backdrop.dataset.mood = cur.dataset.bg || 'grid';
    window.PROMO.backdrop.setMood(backdrop.dataset.mood);

    ticks.forEach((t, k) => t.classList.toggle('is-on', k <= idx));
    counter.textContent = pad2(idx + 1) + ' / ' + pad2(N);
    runSweep();
    syncPlayIcon();
    paint(idx / N);

    window.PROMO.backdrop.readColor();
    window.PROMO.fx.enter(cur);

    const hash = '#' + (idx + 1);
    if (location.hash !== hash) history.replaceState(null, '', hash);
  }

  /* ---------- 一次 rAF 循环：计时 + 进度 + 背景 ---------- */

  function frame(now) {
    raf = requestAnimationFrame(frame);
    if (!last) last = now;
    const ms = now - last;
    last = now;
    const dt = Math.min(ms / 16.667, 3);      // 归一化到 60fps 的「帧数」

    if (playing && !finished) {
      // 首屏、切标签页回来、系统卡顿都可能让单帧 ms 很大；
      // 限幅一下，别让一次卡顿把整页时长直接跑完。
      elapsed += Math.min(ms, 120);
      if (elapsed >= DUR[idx]) {
        if (idx === N - 1) {
          finished = true;
          playing = false;
          elapsed = DUR[idx];
          syncPlayIcon();
        } else {
          go(idx + 1);
        }
      }
    }

    paint((idx + Math.min(elapsed / DUR[idx], 1)) / N);

    if (!document.hidden) window.PROMO.backdrop.draw(dt, reduce);
  }

  /* ---------- 控制 ---------- */

  function togglePlay() {
    if (finished) { playing = true; idx = N - 1; go(0); return; }
    playing = !playing;
    last = performance.now();
    syncPlayIcon();
  }

  btnPrev.addEventListener('click', e => { e.stopPropagation(); go(idx - 1); });
  btnNext.addEventListener('click', e => { e.stopPropagation(); go(idx + 1); });
  btnPlay.addEventListener('click', e => { e.stopPropagation(); togglePlay(); });
  btnFull.addEventListener('click', e => {
    e.stopPropagation();
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen().catch(() => {});
  });

  document.addEventListener('keydown', e => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    switch (e.key) {
      case 'ArrowRight': case 'PageDown': go(idx + 1); break;
      case 'ArrowLeft':  case 'PageUp':   go(idx - 1); break;
      case ' ': case 'k': e.preventDefault(); togglePlay(); break;
      case 'Home': go(0); break;
      case 'End':  go(N - 1); break;
      case 'f': case 'F':
        if (document.fullscreenElement) document.exitFullscreen();
        else document.documentElement.requestFullscreen().catch(() => {});
        break;
    }
  });

  // 像 PPT 一样：点舞台任意处前进；控件与链接除外
  stage.addEventListener('click', e => {
    markInteracted();
    if (e.target.closest('button, a, .rail')) return;
    if (finished) { playing = true; idx = N - 1; go(0); return; }
    go(idx + 1);
  });

  function markInteracted() {
    if (interacted) return;
    interacted = true;
    hint.classList.remove('show');
    hint.classList.add('gone');
  }

  document.addEventListener('visibilitychange', () => { last = performance.now(); });

  /* ---------- 缩放 ---------- */

  function fit() {
    scale = Math.min(window.innerWidth / 1600, window.innerHeight / 900);
    stage.style.transform = 'translate(-50%, -50%) scale(' + scale + ')';
    window.PROMO.backdrop.resize(scale);
  }
  window.addEventListener('resize', fit);
  window.addEventListener('orientationchange', fit);

  /* ---------- 启动 ---------- */

  fit();
  window.PROMO.backdrop.readColor();

  const fromHash = parseInt((location.hash || '').slice(1), 10);
  idx = (Number.isFinite(fromHash) && fromHash >= 1 && fromHash <= N) ? fromHash - 1 : 0;

  slides.forEach((s, k) => {
    const on = k === idx;
    s.classList.toggle('is-active', on);
    s.setAttribute('aria-hidden', on ? 'false' : 'true');
  });
  stage.classList.toggle('is-night', slides[idx].dataset.theme === 'night');
  backdrop.dataset.mood = slides[idx].dataset.bg || 'grid';
  window.PROMO.backdrop.setMood(backdrop.dataset.mood);
  ticks.forEach((t, k) => t.classList.toggle('is-on', k <= idx));
  counter.textContent = pad2(idx + 1) + ' / ' + pad2(N);
  syncPlayIcon();
  window.PROMO.fx.enter(slides[idx]);

  window.PROMO.backdrop.draw(0, true);
  last = performance.now();
  raf = requestAnimationFrame(frame);

  requestAnimationFrame(() => hint.classList.add('show'));
  setTimeout(() => {
    if (!interacted) { hint.classList.remove('show'); hint.classList.add('gone'); }
  }, 7000);

  window.addEventListener('pagehide', () => cancelAnimationFrame(raf));
})();
