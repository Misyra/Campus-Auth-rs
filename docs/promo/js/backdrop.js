/* ============================================================
   背景层：环境信号网格
   46 个缓慢漂移的节点，靠得近就连一条线，另有三条「数据包」沿线爬。
   左疏右密，正文所在的左侧保持干净；颜色跟着主题走（读 currentColor）。
   用经典脚本而不是 ES module：file:// 直接双击也能跑，无需本地服务器。
   ============================================================ */

window.PROMO = window.PROMO || {};

(function (P) {
  'use strict';

  const W = 1600, H = 900;
  const canvas = document.getElementById('mesh');
  const ctx = canvas.getContext('2d');
  const stage = document.getElementById('stage');

  /* 每种「气候」的疏密倍率：安静页真的更空，开场页真的更满 */
  const MOOD = { hero: 1.35, grid: .75, blueprint: .55, streams: .9, bloom: .65, glow: 1, quiet: .4 };
  let moodK = 1;

  const NODES = Array.from({ length: 46 }, (_, i) => ({
    x: Math.random() * W,
    y: Math.random() * H,
    vx: (Math.random() - .5) * .17,
    vy: (Math.random() - .5) * .17,
    r: i % 11 === 0 ? 2.7 : 1 + Math.random() * 1.6,
    hub: i % 11 === 0
  }));

  const PACKETS = Array.from({ length: 4 }, (_, i) => ({
    a: (i * 9) % NODES.length,
    b: (i * 17 + 5) % NODES.length,
    t: Math.random(),
    sp: .0022 + Math.random() * .0016
  }));

  const LINK = 252;
  let rgb = [245, 245, 243];
  let tick = 0;

  /* 从左到右逐渐变密 */
  const density = x => .06 + .94 * Math.pow(Math.max(0, Math.min(1, x / W)), 1.5) * moodK;

  function readColor() {
    const m = getComputedStyle(stage).color.match(/\d+(\.\d+)?/g);
    if (m && m.length >= 3) rgb = [+m[0], +m[1], +m[2]];
  }

  function resize(scale) {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const k = dpr * scale;
    canvas.width = Math.round(W * k);
    canvas.height = Math.round(H * k);
    ctx.setTransform(k, 0, 0, k, 0, 0);
  }

  function draw(dt, still) {
    ctx.clearRect(0, 0, W, H);
    if (++tick % 8 === 1) readColor();
    const r = rgb[0], g = rgb[1], b = rgb[2];

    for (const n of NODES) {
      if (still) break;
      n.x += n.vx * dt; n.y += n.vy * dt;
      if (n.x < -40) n.x = W + 40; else if (n.x > W + 40) n.x = -40;
      if (n.y < -40) n.y = H + 40; else if (n.y > H + 40) n.y = -40;
    }

    ctx.lineWidth = 1;
    for (let i = 0; i < NODES.length; i++) {
      const a = NODES[i];
      for (let j = i + 1; j < NODES.length; j++) {
        const c = NODES[j];
        const dx = a.x - c.x, dy = a.y - c.y;
        const d2 = dx * dx + dy * dy;
        if (d2 > LINK * LINK) continue;
        const alpha = (1 - Math.sqrt(d2) / LINK) * .18 * density((a.x + c.x) / 2);
        if (alpha < .004) continue;
        ctx.strokeStyle = 'rgba(' + r + ',' + g + ',' + b + ',' + alpha.toFixed(3) + ')';
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(c.x, c.y);
        ctx.stroke();
      }
    }

    for (const n of NODES) {
      const alpha = (n.hub ? .5 : .32) * density(n.x);
      if (alpha < .004) continue;
      ctx.fillStyle = 'rgba(' + r + ',' + g + ',' + b + ',' + alpha.toFixed(3) + ')';
      ctx.beginPath();
      ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
      ctx.fill();
    }

    for (const p of PACKETS) {
      const A = NODES[p.a], B = NODES[p.b];
      const len = Math.hypot(A.x - B.x, A.y - B.y);
      if (!still) {
        p.t += p.sp * dt;
        if (p.t > 1) { p.t = 0; p.a = (p.a + 7) % NODES.length; p.b = (p.b + 11) % NODES.length; }
      }
      if (len > LINK * 1.3) continue;
      const x = A.x + (B.x - A.x) * p.t;
      const y = A.y + (B.y - A.y) * p.t;
      const a = density(x);
      ctx.strokeStyle = 'rgba(' + r + ',' + g + ',' + b + ',' + (.1 * a).toFixed(3) + ')';
      ctx.beginPath(); ctx.moveTo(A.x, A.y); ctx.lineTo(B.x, B.y); ctx.stroke();
      ctx.fillStyle = 'rgba(215,147,70,' + (.6 * a).toFixed(3) + ')';
      ctx.beginPath(); ctx.arc(x, y, 2.1, 0, Math.PI * 2); ctx.fill();
    }
  }

  P.backdrop = {
    setMood: function (m) { moodK = MOOD[m] || 1; },
    resize: resize,
    draw: draw,
    readColor: readColor
  };
})(window.PROMO);
