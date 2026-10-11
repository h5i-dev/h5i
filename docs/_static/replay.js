/* h5i landing: the hero stage.
 *
 * Two acts, played in a random order. In one, an agent attacks a running shop
 * from the shell on the right while the code that serves it, on the left, lights
 * up line by line until the missing check shows. In the other, a Rust kernel's
 * rule is stated in Lean and proven, then the same bug is put back on purpose
 * and the proof refuses it. The tmux window list switches acts; the stage stops
 * when off screen, when the tab is hidden or when paused, and with reduced
 * motion it shows one act's last frame.
 */
(function () {
  var stage = document.querySelector('.stage');
  if (!stage) return;
  var $ = function (sel) { return stage.querySelector(sel); };
  var codeBuf = $('.pane-code .pane-buf'), codeSt = $('.pane-code .pane-st span');
  var codePos = $('.pane-code .pane-st .r'), codeMode = $('.pane-code .pane-mode');
  var leanBuf = $('.pane-lean .pane-buf'), shBuf = $('.pane-sh .pane-buf');
  var shLabel = $('.pane-sh .pane-st span'), shRight = $('.pane-sh .pane-st .r');
  var wins = [].slice.call(stage.querySelectorAll('.tmux-win'));
  var toggle = $('.tmux-pause'), info = $('.tmux-info');

  /* ── text ───────────────────────────────────────────────── */
  var KW = {
    ts: 'import from export const async await return if new',
    rs: 'fn match if else let return true false',
    lean: 'def theorem by fun'
  };
  for (var k in KW) KW[k] = KW[k].split(' ');
  function esc(s) { return s.replace(/[&<>]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]; }); }
  function tint(text, lang) {
    return text.replace(/(\/\/.*$|--.*$|'[^']*'|[A-Za-z_][\w.]*|\d+|[^\w\s])/g, function (t) {
      var c = /^(\/\/|--)/.test(t) ? 'vc' : /^'/.test(t) ? 'vs' : KW[lang].indexOf(t) >= 0 ? 'vk'
        : /^[A-Z]/.test(t) ? 'vt' : /^[a-z_]/i.test(t) ? 'vi' : /^\d/.test(t) ? 'vn' : 'vp';
      return '\u0001' + c + '\u0002' + t + '\u0003';
    }).replace(/[&<>]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]; })
      .replace(/\u0001(\w+)\u0002/g, '<span class="$1">').replace(/\u0003/g, '</span>');
  }

  /* ── panes ──────────────────────────────────────────────── */
  function pane(buf, lines, lang, first) {
    buf.innerHTML = '';
    return lines.map(function (text, i) {
      var row = document.createElement('div');
      row.className = 'sl' + (/^\+--/.test(text) ? ' sfold' : '');
      row.innerHTML = '<b class="ss"></b><i>' + (text === null ? '' : first + i) + '</i><span class="sx">' +
        (text ? (/^\+--/.test(text) ? esc(text) : tint(text, lang)) : '') + '</span>';
      buf.appendChild(row);
      return row;
    });
  }
  function row(rows, n, first) { return rows[n - first]; }
  function sign(r, t, cls) { var s = r.querySelector('.ss'); s.textContent = t; s.className = 'ss' + (cls ? ' ' + cls : ''); }
  function focusRow(r) {
    var b = r.parentNode;
    if (r.offsetTop < b.scrollTop + 20 || r.offsetTop > b.scrollTop + b.clientHeight - 40)
      b.scrollTop = r.offsetTop - b.clientHeight / 2;
  }
  // execution heat: the lines that served a request warm up, then cool
  function heat(rs) {
    rs.forEach(function (r) {
      r.classList.remove('cool'); r.classList.add('hot');
      setTimeout(function () { r.classList.add('cool'); r.classList.remove('hot'); }, 700);
    });
    if (rs[0]) focusRow(rs[0]);
  }
  // find a run of text in a row and wrap it, so it can be marked or rewritten
  function grab(r, text) {
    var sx = r.querySelector('.sx'), plain = sx.textContent, at = plain.indexOf(text);
    if (at < 0) return null;
    var before = plain.slice(0, at), after = plain.slice(at + text.length), lang = r.dataset.lang;
    sx.innerHTML = tint(before, lang) + '<span class="grab">' + tint(text, lang) + '</span>' + tint(after, lang);
    return sx.querySelector('.grab');
  }

  /* ── the shell ──────────────────────────────────────────── */
  var sh = { lines: [], cur: null };
  function runs(rs) { return rs.map(function (r) { return '<span class="t' + r[0] + '">' + esc(r[1]) + '</span>'; }).join(''); }
  function shDraw() {
    var html = sh.lines.slice(-40).map(function (l) { return '<div>' + (runs(l) || ' ') + '</div>'; });
    if (sh.cur) html.push('<div>' + runs(sh.cur) + '<span class="cur"></span></div>');
    shBuf.innerHTML = html.join('');
    shBuf.scrollTop = shBuf.scrollHeight;
  }
  function cmdRuns(text, note) {
    var out = [['p', '$ ']];
    text.split(/( )/).forEach(function (w, i) { out.push([i < 3 ? 'c' : /^--/.test(w) ? 'f' : 'a', w]); });
    if (note) out.push(['m', '   ' + note]);
    return out;
  }

  /* ── time ───────────────────────────────────────────────── */
  var paused = false, visible = true, fast = false, gen = 0;
  function Stop() {}
  function sleep(ms) {
    var g = gen;
    if (fast) return Promise.resolve();
    return new Promise(function (done, fail) {
      var left = ms, last = performance.now();
      (function tick() {
        if (g !== gen) return fail(new Stop());
        var now = performance.now();
        if (!(paused || !visible || document.hidden)) left -= now - last;
        last = now;
        if (left <= 0) done(); else setTimeout(tick, Math.min(left, 50));
      })();
    });
  }
  function rand(a, b) { return a + Math.random() * (b - a); }
  var NEAR = {};
  ['qwertyuiop', 'asdfghjkl', 'zxcvbnm'].forEach(function (r) {
    for (var i = 0; i < r.length; i++) NEAR[r[i]] = (r[i - 1] || '') + (r[i + 1] || '');
  });

  async function type(cmd, note) {
    stage.classList.add('typing');
    var slip = Math.random() < 0.5 ? Math.floor(rand(8, cmd.length - 3)) : -1;
    if (!NEAR[cmd[slip]]) slip = -1;
    for (var i = 0; i <= cmd.length; i++) {
      if (i === slip) {
        var n = NEAR[cmd[i]];
        sh.cur = cmdRuns(cmd.slice(0, i) + n[Math.floor(Math.random() * n.length)]); shDraw();
        await sleep(rand(260, 420));
        sh.cur = cmdRuns(cmd.slice(0, i)); shDraw();
        await sleep(rand(90, 160));
      }
      sh.cur = cmdRuns(cmd.slice(0, i)); shDraw();
      await sleep(cmd[i - 1] === ' ' ? rand(60, 120) : rand(20, 52));
    }
    if (note) { await sleep(rand(150, 250)); sh.cur = cmdRuns(cmd, note); shDraw(); }
    stage.classList.remove('typing');
    await sleep(rand(240, 380));
    sh.lines.push(sh.cur); sh.cur = null; shDraw();
  }
  var SPIN = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
  async function spin(ms) {
    for (var t = 0, f = 0; t < ms && !fast; t += 80, f++) { sh.cur = [['d', '  ' + SPIN[f % 10]]]; shDraw(); await sleep(80); }
    sh.cur = null; shDraw();
  }
  async function out(ls, gap) {
    for (var i = 0; i < ls.length; i++) { sh.lines.push(ls[i]); shDraw(); await sleep(gap || rand(50, 100)); }
  }
  function prompt() { sh.cur = [['p', '$ ']]; shDraw(); }

  // letters fall through glyphs before they settle, after hancore's wall
  var GLYPHS = '░▒▓/\\<>+=*#%&@$01';
  async function scramble(el, to, cls) {
    var from = el.textContent, n = Math.max(from.length, to.length);
    el.className = 'grab ' + cls;
    for (var f = 0; f < 14 && !fast; f++) {
      var s = '';
      for (var i = 0; i < n; i++) s += i < n * f / 14 ? (to[i] || '') : GLYPHS[Math.floor(Math.random() * GLYPHS.length)];
      el.textContent = s;
      await sleep(45);
    }
    el.textContent = to;
  }
  async function virt(r, text, cls) {
    var v = document.createElement('span');
    v.className = 'virt ' + (cls || '');
    r.querySelector('.sx').appendChild(v);
    for (var i = 1; i <= text.length; i++) { v.textContent = text.slice(0, i); if (i % 2 === 0) await sleep(18); }
    return v;
  }

  /* ── wires ───────────────────────────────────────────────────
   * What h5i records is evidence that points at something, so the stage draws
   * the pointing: a wire from the command or the theorem to the line of code it
   * reached, with the request riding along it. A finding pins its wire red; a
   * proof that stops building breaks its wire. One SVG, moved only on events.
   */
  var wires = (function () {
    var NS = 'http://www.w3.org/2000/svg', host = $('.stage-panes');
    var svg = document.createElementNS(NS, 'svg');
    svg.setAttribute('class', 'wires'); svg.setAttribute('aria-hidden', 'true');
    host.appendChild(svg);
    function el(name, attrs, parent) {
      var n = document.createElementNS(NS, name);
      for (var k in attrs) n.setAttribute(k, attrs[k]);
      (parent || svg).appendChild(n);
      return n;
    }
    function box(node) {
      var h = host.getBoundingClientRect(), r = node.getBoundingClientRect();
      return { l: r.left - h.left, r: r.right - h.left, y: (r.top + r.bottom) / 2 - h.top };
    }
    // where a code row's text ends, so the wire lands on the code and not the gutter
    function textEnd(row) {
      var sx = row.querySelector('.sx') || row, rg = document.createRange();
      rg.selectNodeContents(sx);
      var h = host.getBoundingClientRect(), r = rg.getBoundingClientRect();
      if (!r.width) r = sx.getBoundingClientRect();
      return { x: Math.min(r.right - h.left + 8, host.clientWidth / 2 - 8), y: (r.top + r.bottom) / 2 - h.top };
    }
    function textStart(row) {
      var b = box(row);
      return { x: b.l + 1, y: b.y };
    }
    function curve(f, t) {
      var dx = Math.max(40, Math.abs(f.x - t.x) * 0.45);
      return 'M' + f.x.toFixed(1) + ' ' + f.y.toFixed(1) + 'C' + (f.x - dx).toFixed(1) + ' ' + f.y.toFixed(1) + ' ' +
        (t.x + dx).toFixed(1) + ' ' + t.y.toFixed(1) + ' ' + t.x.toFixed(1) + ' ' + t.y.toFixed(1);
    }
    // stacked panes (a phone) and reduced motion get no wires; the code still lights up
    var NONE = { to: function () {}, set: function () {}, fade: function () {} };
    function wire(f, t, tag, cls) {
      if (fast || host.clientWidth < 760) return NONE;
      var gr = el('g', { class: 'wire-g ' + (cls || '') });
      var path = el('path', { class: 'wire', d: curve(f, t), pathLength: 1 }, gr);
      var end = el('circle', { class: 'wire-end', cx: t.x, cy: t.y, r: 2.5 }, gr);
      // the tag sits on the middle of the wire, in the gutter between the panes, off the code
      var label = el('text', { class: 'wire-tag', 'text-anchor': 'middle' }, gr);
      label.textContent = tag || '';
      function place() { var m = path.getPointAtLength(path.getTotalLength() / 2); label.setAttribute('x', m.x); label.setAttribute('y', m.y - 6); }
      place();
      requestAnimationFrame(function () { path.classList.add('on'); });
      // the request rides the wire from the shell to the code
      var dot = el('circle', { class: 'packet', r: 3 }, gr), len = path.getTotalLength(), t0 = performance.now();
      (function ride(now) {
        var k = Math.min(1, (now - t0) / 650), p = path.getPointAtLength(len * (1 - Math.pow(1 - k, 3)));
        dot.setAttribute('cx', p.x); dot.setAttribute('cy', p.y);
        if (k < 1) requestAnimationFrame(ride); else dot.remove();
      })(t0);
      var w = {
        to: function (t2, tag2) {
          path.setAttribute('d', curve(f, t2)); end.setAttribute('cx', t2.x); end.setAttribute('cy', t2.y);
          place();
          if (tag2 !== undefined) label.textContent = tag2;
          len = path.getTotalLength();
        },
        set: function (c, tag2) { gr.setAttribute('class', 'wire-g ' + c); if (tag2 !== undefined) label.textContent = tag2; },
        fade: function (ms) { setTimeout(function () { gr.classList.add('gone'); setTimeout(function () { gr.remove(); }, 700); }, ms || 0); }
      };
      return w;
    }
    return {
      // from the shell line that sent it to the code row that served it
      request: function (shLine, row, tag, cls) {
        if (fast || host.clientWidth < 760) return NONE;
        var b = box(shLine);
        return wire({ x: b.l - 6, y: b.y }, textEnd(row), tag, cls);
      },
      // from a theorem's text to the code row it covers
      proof: function (thmRow, row, tag, cls) {
        if (fast || host.clientWidth < 760) return NONE;
        return wire(textStart(thmRow), textEnd(row), tag, cls);
      },
      end: textEnd,
      clear: function () { while (svg.firstChild) svg.removeChild(svg.firstChild); }
    };
  })();
  function lastCmd() {
    // the newest command line on screen: one that starts with a prompt and has a command after it
    for (var k = shBuf.children.length - 1; k >= 0; k--) {
      var d = shBuf.children[k];
      if (d.querySelector('.tp') && d.querySelector('.tc')) return d;
    }
    return shBuf;
  }

  /* ── the eye ────────────────────────────────────────────────
   * A line drawing, not a picture: two lids, an aperture of hairlines for an
   * iris, and the red dot of the h5i mark for a pupil. It opens when h5i starts
   * looking, turns toward the line being exercised, and locks red on a bug.
   * Everything that moves is a CSS transition; the script only says where.
   */
  var eye = (function () {
    var NS = 'http://www.w3.org/2000/svg', host = $('.pane-code'), buf = codeBuf;
    var W = 250, H = 122, A = W * 0.47, RI = 35;
    function el(name, attrs, parent) {
      var n = document.createElementNS(NS, name);
      for (var k in attrs) n.setAttribute(k, attrs[k]);
      if (parent) parent.appendChild(n);
      return n;
    }
    // the lids, as two strokes: a heavy upper one and a light lower one
    function lidD(upper, from, to) {
      var d = '';
      for (var i = from; from <= to ? i <= to : i >= to; i += from <= to ? 1 : -1) {
        var u = i / 12 - 1, y = upper ? -H * 0.47 * Math.pow(1 - u * u, 0.8) : H * 0.37 * Math.pow(1 - u * u, 1.15);
        d += (d ? 'L' : 'M') + (u * A).toFixed(1) + ' ' + y.toFixed(1);
      }
      return d;
    }
    var upperD = lidD(true, 0, 24), lowerD = lidD(false, 24, 0);
    var almond = upperD + lowerD.replace(/^M/, 'L') + 'Z';
    var svg = el('svg', { class: 'eye-svg', 'aria-hidden': 'true' });
    var defs = el('defs', {}, svg), clip = el('clipPath', { id: 'eye-clip' }, defs);
    el('path', { d: almond }, clip);
    // the lids are lines of code bent to the eye's contour, fading out at the corners into the code around them
    var fade = el('linearGradient', { id: 'eye-fade', gradientUnits: 'userSpaceOnUse', x1: -A - 10, x2: A + 10, y1: 0, y2: 0 }, defs);
    [[0, 0], [0.22, 0.85], [0.78, 0.85], [1, 0]].forEach(function (st) {
      el('stop', { class: 'eye-stop', offset: st[0], 'stop-opacity': st[1] }, fade);
    });
    el('path', { id: 'eye-up', d: upperD }, defs);
    el('path', { id: 'eye-brow', d: upperD.replace(/(-?[\d.]+) (-?[\d.]+)/g, function (m, x, y) { return x + ' ' + (+y * 1.12 - 7).toFixed(1); }) }, defs);
    el('path', { id: 'eye-lo', d: lidD(false, 0, 24) }, defs);
    el('path', { id: 'eye-iris-path', d: 'M0 ' + (-(RI - 5)) + 'a' + (RI - 5) + ' ' + (RI - 5) + ' 0 1 1 -0.01 0' }, defs);
    var ray = el('path', { class: 'eye-ray', pathLength: 1 }, svg);
    var tag = el('text', { class: 'eye-tag' }, svg);
    var at = el('g', { class: 'eye-at' }, svg);
    var lid = el('g', { class: 'eye-lid' }, at);
    el('path', { class: 'eye-bg', d: almond }, lid);
    var inner = el('g', { 'clip-path': 'url(#eye-clip)' }, lid);
    // the lens: the pane's code, magnified, scrolled so the line being read sits in the middle
    var lensRows = [], STEP = 16;
    var iris = el('g', { class: 'eye-iris' }, inner);
    el('circle', { class: 'eye-disc', r: RI }, iris);
    // the iris spells what the eye is checking, and turns
    var ticks = el('g', { class: 'eye-ticks' }, iris);
    var ringText = el('textPath', { href: '#eye-iris-path' }, el('text', { class: 'eye-iris-text' }, ticks));
    el('circle', { class: 'eye-ring', r: RI }, iris);
    var pulse = el('circle', { class: 'eye-pulse', r: RI }, iris);
    el('circle', { class: 'eye-ring inner', r: 11 }, iris);
    el('circle', { class: 'eye-pupil', r: 8 }, iris);
    el('circle', { class: 'eye-glint', r: 2, cx: -11, cy: -12 }, iris);
    // the line being read is drawn over the iris, so the eye reads it whole
    var lens = el('g', { class: 'eye-lens' }, inner);
    var lidText = ['eye-brow', 'eye-up', 'eye-lo'].map(function (id) {
      return el('textPath', { href: '#' + id }, el('text', { class: 'eye-lid-text ' + id }, lid));
    });
    host.appendChild(svg);

    var cx = 0, cy = 0, target = null, timer = 0, on = false, lensKind = 'code';
    function place(fy) {
      var top = buf.offsetTop, w = host.clientWidth;
      svg.style.top = top + 'px'; svg.style.height = buf.clientHeight + 'px';
      cx = w - W / 2 - 44; cy = buf.clientHeight * fy;
      at.setAttribute('transform', 'translate(' + cx.toFixed(1) + ' ' + cy.toFixed(1) + ')');
    }
    function rowPoint(r) {
      var sx = r.querySelector('.sx'), rg = document.createRange();
      rg.selectNodeContents(sx);
      var b = rg.getBoundingClientRect(), h = svg.getBoundingClientRect();
      if (!b.width) b = sx.getBoundingClientRect();
      return [b.right - h.left + 6, (b.top + b.bottom) / 2 - h.top];
    }
    function gaze(p) {
      var dx = p ? p[0] - cx : 0, dy = p ? p[1] - cy : 0, n = Math.hypot(dx, dy) || 1;
      var k = Math.min(1, n / 160);
      iris.style.transform = 'translate(' + (dx / n * 38 * k).toFixed(1) + 'px,' + (dy / n * 9 * k).toFixed(1) + 'px)';
    }
    // the lens shows the code under the eye, or whatever it is handed: a raw request and its response
    function fill(lines) {
      while (lens.firstChild) lens.removeChild(lens.firstChild);
      lines = lines || [].map.call(buf.children, function (r) { return (r.querySelector('.sx') || r).textContent; });
      lensRows = lines.map(function (line, i) {
        var t = el('text', { x: -A + 14, y: i * STEP + 4 }, lens);
        t.textContent = line;
        return t;
      });
    }
    function read(i) {
      if (!lensRows[i]) return;
      lensRows.forEach(function (t, k) { t.classList.toggle('cur', k === i); });
      // indentation is scrolled away, so the line being read starts near the corner and runs across the eye
      var indent = (lensRows[i].textContent.match(/^ */) || [''])[0].length;
      var tx = 8 - indent * 7.5;
      lens.style.transform = 'translate(' + tx.toFixed(1) + 'px,' + (-i * STEP) + 'px)';
    }
    function rowIndex(r) { return [].indexOf.call(buf.children, r); }
    // when there is nothing to follow it reads around, in short glances
    function glance() {
      clearTimeout(timer);
      if (!on) return;
      if (!target && !svg.classList.contains('lock') && !(paused || !visible || document.hidden)) {
        gaze([cx + (Math.random() * 2 - 1) * 200, cy + (Math.random() * 2 - 1) * 120]);
        if (lensKind === 'code' && lensRows.length) read(Math.floor(Math.random() * lensRows.length));
        if (Math.random() < 0.15) blink();
      }
      timer = setTimeout(glance, 700 + Math.random() * 900);
    }
    function blink() {
      svg.classList.add('blink');
      setTimeout(function () { svg.classList.remove('blink'); }, 140);
    }
    var off = !!(fast || matchMedia('(prefers-reduced-motion: reduce)').matches);
    return {
      show: function (fy, ring) {
        if (off || host.clientWidth < 520) return;
        place(fy || 0.72); on = true; target = null;
        svg.classList.remove('lock'); svg.classList.add('open');
        lensKind = 'code'; fill(); gaze(null); glance();
        var src = [].map.call(buf.querySelectorAll('.sx'), function (x) { return x.textContent.trim(); }).filter(Boolean).join('  '), at0 = Math.floor(Math.random() * Math.max(1, src.length - 200));
        lidText.forEach(function (t, k) { t.textContent = (src + ' ' + src).slice(at0 + k * 61, at0 + k * 61 + 70); });
        ringText.textContent = ring || '';
      },
      look: function (r) {
        if (!on) return;
        target = r || null;
        gaze(r ? rowPoint(r) : null);
        if (r && lensKind === 'code') read(rowIndex(r));
        if (!r) glance();
      },
      // hand the lens something to read: a request and its response, at line i
      feed: function (lines, i) {
        if (!on) return;
        lensKind = 'feed'; fill(lines); read(i || 0);
      },
      read: function (i) { if (on) read(i); },
      // lock red: on a code row (with a sight line to it), or on line i of what the lens was fed
      lock: function (r, text, i) {
        if (!on) return;
        target = r;
        tag.textContent = text || '';
        if (r) {
          var p = rowPoint(r), q = [cx, cy];
          gaze(p);
          lensKind = 'code'; fill(); read(rowIndex(r));
          ray.setAttribute('d', 'M' + q[0].toFixed(1) + ' ' + q[1].toFixed(1) + 'L' + p[0].toFixed(1) + ' ' + p[1].toFixed(1));
          tag.setAttribute('x', ((p[0] + q[0]) / 2).toFixed(1)); tag.setAttribute('y', ((p[1] + q[1]) / 2 - 7).toFixed(1));
          svg.classList.remove('lock', 'nosight');
        } else {
          read(i || 0);
          tag.setAttribute('x', cx.toFixed(1)); tag.setAttribute('y', (cy - H * 0.47 - 10).toFixed(1));
          svg.classList.remove('lock'); svg.classList.add('nosight');
        }
        void svg.getBBox(); svg.classList.add('lock');
      },
      unlock: function () { svg.classList.remove('lock'); target = null; glance(); },
      hide: function () { on = false; clearTimeout(timer); svg.classList.remove('open', 'lock'); },
      reset: function () { on = false; clearTimeout(timer); svg.classList.remove('open', 'lock', 'blink'); }
    };
  })();

  /* ── act 1: find vulnerabilities ────────────────────────── */
  // what the lens reads in the first act: the order request, then its replay
  function traffic(id, owner, total) {
    return ['GET /api/order/' + id + ' HTTP/1.1', 'host: shop.example.test', 'cookie: sid=9f2c…  (u_17)', '',
            'HTTP/1.1 200 OK', '{ "id": ' + id + ',', '  "owner": "' + owner + '",', '  "total": ' + total + ' }'];
  }
  var TS = [
    "import { Router } from 'express';",
    "import { db } from '../db';",
    "import { requireLogin } from '../auth';",
    "import { pay } from '../billing';",
    'export const router = Router();',
    '',
    "router.get('/checkout', requireLogin, async (req, res) => {",
    '  const orders = await db.order.findMany({',
    '    where: { userId: req.user.id },',
    '  });',
    "  res.render('checkout', { orders });",
    '});',
    '',
    "router.get('/api/order/:id', requireLogin, async (req, res) => {",
    '  const order = await db.order.findUnique({',
    '    where: { id: Number(req.params.id) },',
    '  });',
    '  if (!order) return res.status(404).end();',
    '  res.json(order);',
    '});',
    '',
    "router.post('/api/order/:id/pay', requireLogin, async (req, res) => {",
    '  const order = await db.order.findFirst({',
    '    where: { id: Number(req.params.id), userId: req.user.id },',
    '  });',
    '  if (!order) return res.status(404).end();',
    '  res.json(await pay(order));',
    '});'
  ];
  async function find() {
    stage.dataset.act = 'find'; wires.clear(); eye.reset();
    codeSt.textContent = 'routes/orders.ts'; codeMode.textContent = 'NORMAL'; codePos.textContent = 'ts · 1:1';
    shLabel.textContent = 'agent@box: ~/shop-audit';
    var rs = pane(codeBuf, TS, 'ts', 1);
    rs.forEach(function (r) { r.dataset.lang = 'ts'; });
    var L = function (n) { return row(rs, n, 1); };
    var count = 205;
    function rec(n) { count += n; shRight.textContent = count + ' recorded'; }
    sh.lines = []; prompt(); rec(0); info.textContent = 'shop.example.test';
    await sleep(700);

    await type('h5i browser open https://shop.example.test --capture --session checkout-idor');
    await spin(700);
    heat([7, 8, 9, 10, 11].map(L)); rec(2);
    wires.request(lastCmd(), L(7), 'GET /checkout').fade(1800);
    await out([[['o', '  '], ['k', 'checkout-idor'], ['d', '  live · engine h5i · capture on']]]);
    prompt(); await sleep(900);

    await type('h5i browser click @e7', '# open order #1041');
    heat([14, 15, 16, 17, 18, 19].map(L)); rec(3);
    wires.request(lastCmd(), L(14), 'req_108  GET /api/order/1041').fade(1800);
    eye.show(0.74, 'req_108 · res_109 · owner · cookie u_17 · '); eye.feed(traffic(1041, 'u_17', '42.00'), 0);
    await out([[['o', '  → /checkout?order=1041  '], ['d', '200 · 3 requests']]]);
    eye.read(6);
    prompt(); await sleep(900);

    await type('h5i websec requests --human', '# what the session sent');
    await out([[['o', '  #107  GET  /checkout?order=1041     200  2.1 kB']],
               [['o', '  #108  GET  /api/order/1041          200  1.4 kB']],
               [['o', '  #112  GET  ps.example.invalid/t.js  '], ['r', 'refused'], ['d', '  out of scope']]]);
    prompt(); await sleep(1000);

    await type('h5i websec replay req_108 --set path=/api/order/1042');
    await spin(500);
    heat([14, 15, 16, 17, 18, 19].map(L)); rec(1);
    var probe = wires.request(lastCmd(), L(16), 'req_109  id = 1042');
    eye.feed(traffic(1042, 'u_23', '118.50'), 0);
    await out([[['o', '  res_109  200  1.9 kB']]]);
    eye.read(6);
    prompt(); await sleep(800);

    await type('h5i websec diff res_108 res_109 --human');
    await out([[['o', '  status : 200 → 200']], [['o', '  bytes  : 1412 → 1903 (+491)']]]);
    await sleep(500);
    await out([[['o', '  owner  : u_17 → '], ['r', 'u_23'], ['d', '   another customer\'s order']]]);
    // the wire that carried the replay is pinned red: this is the line that answered it
    probe.set('red', 'u_23 ≠ u_17');
    // what came back first, in the traffic; then why, in the code
    eye.lock(null, 'u_23 is not the caller', 6);
    await sleep(1600);
    eye.lock(L(16), 'no owner in the query');
    await sleep(900);
    // the reveal: the query that never asked whose order it was
    codeMode.textContent = 'FINDING'; stage.classList.add('alarm');
    var bad = L(16); bad.classList.add('bad'); focusRow(bad); codePos.textContent = 'ts · 16:12';
    var g = grab(bad, 'where: { id: Number(req.params.id) }');
    if (g) g.classList.add('under');
    sign(bad, '✗', 'no');
    await sleep(500);
    await virt(bad, '   ◀ any logged-in user reads any order', 'no');
    await sleep(700);
    var good = L(9), gg = grab(good, 'userId: req.user.id');
    if (gg) gg.classList.add('ok');
    sign(good, '✓', 'ok');
    await virt(good, '   ◀ this route asks whose it is', 'ok');
    prompt(); await sleep(1400);

    probe.fade(); eye.hide();
    await type('h5i websec finding create --title "cross-user order read" --evidence req_108,res_109');
    await out([[['o', '  '], ['k', 'finding_1'], ['o', '  recorded · cites req_108, res_109']]]);
    prompt();
    await sleep(6000);
  }

  /* ── act 2: prove correctness ───────────────────────────── */
  var RS = [
    '/// One order, to its owner and to no one else.',
    '',
    'fn read_order(user: u64, s: &Snapshot, id: u64) -> Outcome {',
    '    match find_order(&s.orders, id) {',
    '        None => Err(Error::NotFound),',
    '        Some(o) => {',
    '            if o.owner == user {',
    '                Ok((none(), Reply::Order(o.clone())))',
    '            } else {',
    '                Err(Error::Forbidden)',
    '            }',
    '        }',
    '    }',
    '}',
    '',
    'fn list_orders(user: u64, s: &Snapshot) -> Outcome {',
    '    let mine = s.orders.iter().filter(|o| o.owner == user);',
    '    Ok((none(), Reply::Orders(mine.cloned().collect())))',
    '}',
    '',
    'fn cancel_order(user: u64, s: &Snapshot, id: u64) -> Outcome {',
    '    match find_order(&s.orders, id) {',
    '        Some(o) if o.owner == user && o.state == State::Open =>',
    '            Ok((one(Write::Cancel(id)), Reply::Done)),',
    '        Some(_) => Err(Error::Forbidden),',
    '        None => Err(Error::NotFound),',
    '    }',
    '}'
  ];
  var LEAN = [
    'def visible (s : St) (u : Nat) : Reply → Prop',
    '  | .Order o => o.owner.val = u',
    '  | _ => True',
    '',
    'theorem owner_only (a : Principal) (s : Snapshot) (c : Command) ws r',
    '    (h : transition a s c = .ok (.Ok (ws, r))) :',
    '    visible (Snapshot.toSt s) a.user.val r := by',
    '+-- 14 lines: rcases reply_of a s c ws r h with ·····'
  ];
  async function prove() {
    stage.dataset.act = 'prove'; wires.clear(); eye.reset();
    codeSt.textContent = 'kernel/src/lib.rs'; codeMode.textContent = 'NORMAL';
    shLabel.textContent = 'agent@box: ~/shop';
    var rs = pane(codeBuf, RS, 'rs', 38);
    codePos.textContent = 'rs · 44:16';
    rs.forEach(function (r) { r.dataset.lang = 'rs'; });
    shRight.textContent = 'lean 4'; info.textContent = 'kernel · proofs';
    sh.lines = []; prompt();
    // the rule, written as it is stated
    var ls = pane(leanBuf, LEAN.map(function () { return ''; }), 'lean', 12);
    await sleep(600);
    for (var i = 0; i < LEAN.length; i++) {
      var sx = ls[i].querySelector('.sx'), t = LEAN[i];
      if (/^\+--/.test(t)) { ls[i].classList.add('sfold'); sx.textContent = t; continue; }
      for (var j = 1; j <= t.length; j += 2) { sx.innerHTML = tint(t.slice(0, j), 'lean'); await sleep(14); }
      sx.innerHTML = tint(t, 'lean');
      await sleep(90);
    }
    var thm = [4, 5, 6].map(function (n) { return ls[n]; });
    await sleep(700);

    await type('h5i app prove', '# extract, build, then the gate');
    await out([[['d', '  extract  kernel → proofs/generated/']], [['d', '  lake     build Theorems']]], 220);
    // the proof walks the kernel; every line it covers is signed
    codeMode.textContent = 'PROVING';
    var pw = wires.proof(thm[0], rs[2], 'owner_only', 'ok');
    eye.show(0.8, 'owner_only · inv_preserved · read_order · ');
    await sleep(500);
    for (var n = 2; n < 14; n++) {
      rs[n].classList.add('scan'); sign(rs[n], '✓', 'ok'); pw.to(wires.end(rs[n])); eye.look(rs[n]);
      await sleep(150);
      rs[n].classList.remove('scan');
    }
    thm.forEach(function (r) { sign(r, '✓', 'ok'); });
    pw.fade(900); eye.look(null);
    codeMode.textContent = 'NORMAL';
    await out([[['o', '  owner_only          '], ['g', 'proven']], [['o', '  inv_preserved       '], ['g', 'proven']]], 160);
    prompt(); await sleep(1800);

    await type('h5i app mutate --auto', '# put a bug in, on purpose');
    var line = rs[6], g = grab(line, 'o.owner == user');
    codeMode.textContent = 'MUTANT'; stage.classList.add('alarm'); focusRow(line);
    rs.forEach(function (r) { sign(r, ''); });
    await scramble(g, 'true', 'mut');
    eye.lock(line, 'if true');
    await virt(line, '   ◀ any user reads any order', 'no');
    await out([[['o', '  mutant   if '], ['k', 'o.owner == user'], ['d', '  →  '], ['r', 'if true']]]);
    await spin(900);
    await out([[['d', '  lake     build Theorems']]]);
    await sleep(600);
    thm.forEach(function (r) { sign(r, '✗', 'no'); r.classList.add('bad'); });
    // the proof no longer reaches the kernel: its wire comes back broken
    var bw = wires.proof(thm[0], line, 'owner_only  ✗', 'red broken');
    sign(line, '✗', 'no');
    await out([[['o', '  owner_only          '], ['r', 'fails'], ['d', '   the bug cannot hide']],
               [['o', '  mutant   '], ['g', 'caught']]], 300);
    await sleep(1800);
    // the kernel comes back, and so do the proofs
    line.querySelector('.virt').remove();
    bw.fade(); eye.unlock();
    await scramble(g, 'o.owner == user', '');
    g.className = 'grab';
    stage.classList.remove('alarm'); codeMode.textContent = 'NORMAL';
    thm.forEach(function (r) { sign(r, '✓', 'ok'); r.classList.remove('bad'); });
    rs.slice(2, 14).forEach(function (r) { sign(r, '✓', 'ok'); });
    await out([[['k', '  14 of 14 mutants caught']]]);
    eye.hide();
    prompt();
    await sleep(6000);
  }

  /* ── direction ──────────────────────────────────────────── */
  var ACTS = { find: find, prove: prove };
  var order = Math.random() < 0.5 ? ['find', 'prove'] : ['prove', 'find'];
  var next = order[0];
  var lines = [].slice.call(document.querySelectorAll('.act-line'));
  function mark(name) {
    lines.forEach(function (l) { l.classList.toggle('on', l.dataset.act === name); });
    wins.forEach(function (w) {
      var on = w.dataset.act === name;
      w.classList.toggle('on', on);
      w.setAttribute('aria-pressed', on ? 'true' : 'false');
    });
  }
  async function loop() {
    for (;;) {
      var name = next, g = gen;
      next = order[(order.indexOf(name) + 1) % 2];
      stage.classList.remove('alarm');
      mark(name);
      try { await ACTS[name](); } catch (e) { if (!(e instanceof Stop)) throw e; }
      if (g !== gen) return;  // a click started a new run
    }
  }
  function start(name) { gen++; next = name; loop(); }
  wins.forEach(function (w) {
    w.addEventListener('click', function () { if (!fast) start(w.dataset.act); });
  });
  toggle.addEventListener('click', function () {
    paused = !paused;
    stage.classList.toggle('paused', paused);
    toggle.textContent = paused ? 'Play' : 'Pause';
    toggle.setAttribute('aria-pressed', paused ? 'true' : 'false');
  });
  if ('IntersectionObserver' in window) {
    new IntersectionObserver(function (es) { visible = es[0].isIntersecting; }).observe(stage);
  }
  stage.classList.add('live');
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
    fast = true; toggle.hidden = true;
    mark('find'); find();
    return;
  }
  start(next);
})();
