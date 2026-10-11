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

  /* ── act 1: find vulnerabilities ────────────────────────── */
  var TS = [
    "import { Router } from 'express';",
    "import { db } from '../db';",
    "import { requireLogin } from '../auth';",
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
    '});'
  ];
  async function find() {
    stage.dataset.act = 'find';
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
    heat([6, 7, 8, 9, 10].map(L)); rec(2);
    await out([[['o', '  '], ['k', 'checkout-idor'], ['d', '  live · engine h5i · capture on']]]);
    prompt(); await sleep(900);

    await type('h5i browser click @e7', '# open order #1041');
    heat([13, 14, 15, 16, 17, 18].map(L)); rec(3);
    await out([[['o', '  → /checkout?order=1041  '], ['d', '200 · 3 requests']]]);
    prompt(); await sleep(900);

    await type('h5i websec requests --human', '# what the session sent');
    await out([[['o', '  #107  GET  /checkout?order=1041     200  2.1 kB']],
               [['o', '  #108  GET  /api/order/1041          200  1.4 kB']],
               [['o', '  #112  GET  ps.example.invalid/t.js  '], ['r', 'refused'], ['d', '  out of scope']]]);
    prompt(); await sleep(1000);

    await type('h5i websec replay req_108 --set path=/api/order/1042');
    await spin(500);
    heat([13, 14, 15, 16, 17, 18].map(L)); rec(1);
    await out([[['o', '  res_109  200  1.9 kB']]]);
    prompt(); await sleep(800);

    await type('h5i websec diff res_108 res_109 --human');
    await out([[['o', '  status : 200 → 200']], [['o', '  bytes  : 1412 → 1903 (+491)']]]);
    await sleep(500);
    await out([[['o', '  owner  : u_17 → '], ['r', 'u_23'], ['d', '   another customer\'s order']]]);
    // the reveal: the query that never asked whose order it was
    codeMode.textContent = 'FINDING'; stage.classList.add('alarm');
    var bad = L(15); bad.classList.add('bad'); focusRow(bad); codePos.textContent = 'ts · 15:12';
    var g = grab(bad, 'where: { id: Number(req.params.id) }');
    if (g) g.classList.add('under');
    sign(bad, '✗', 'no');
    await sleep(500);
    await virt(bad, '  ◀ any logged-in user reads any order', 'no');
    await sleep(700);
    var good = L(8), gg = grab(good, 'userId: req.user.id');
    if (gg) gg.classList.add('ok');
    sign(good, '✓', 'ok');
    await virt(good, '  ◀ this route asks whose it is', 'ok');
    prompt(); await sleep(1400);

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
    stage.dataset.act = 'prove';
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
    for (var n = 2; n < 14; n++) {
      rs[n].classList.add('scan'); sign(rs[n], '✓', 'ok');
      await sleep(110);
      rs[n].classList.remove('scan');
    }
    thm.forEach(function (r) { sign(r, '✓', 'ok'); });
    codeMode.textContent = 'NORMAL';
    await out([[['o', '  owner_only          '], ['g', 'proven']], [['o', '  inv_preserved       '], ['g', 'proven']]], 160);
    prompt(); await sleep(1800);

    await type('h5i app mutate --auto', '# put a bug in, on purpose');
    var line = rs[6], g = grab(line, 'o.owner == user');
    codeMode.textContent = 'MUTANT'; stage.classList.add('alarm'); focusRow(line);
    rs.forEach(function (r) { sign(r, ''); });
    await scramble(g, 'true', 'mut');
    await virt(line, '  ◀ any user reads any order', 'no');
    await out([[['o', '  mutant   if '], ['k', 'o.owner == user'], ['d', '  →  '], ['r', 'if true']]]);
    await spin(900);
    await out([[['d', '  lake     build Theorems']]]);
    await sleep(600);
    thm.forEach(function (r) { sign(r, '✗', 'no'); r.classList.add('bad'); });
    sign(line, '✗', 'no');
    await out([[['o', '  owner_only          '], ['r', 'fails'], ['d', '   the bug cannot hide']],
               [['o', '  mutant   '], ['g', 'caught']]], 300);
    await sleep(1800);
    // the kernel comes back, and so do the proofs
    line.querySelector('.virt').remove();
    await scramble(g, 'o.owner == user', '');
    g.className = 'grab';
    stage.classList.remove('alarm'); codeMode.textContent = 'NORMAL';
    thm.forEach(function (r) { sign(r, '✓', 'ok'); r.classList.remove('bad'); });
    rs.slice(2, 14).forEach(function (r) { sign(r, '✓', 'ok'); });
    await out([[['k', '  14 of 14 mutants caught']]]);
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
/* The proof figure: the same rule in the code and in the spec, the proofs
 * going through, then a mutant that has to break one of them. */
(function () {
  var fig = document.querySelector('.vim');
  if (!fig) return;
  var out = fig.querySelector('.vim-out');
  var cmd = fig.querySelector('.vim-cmdline');
  var mode = fig.querySelector('.vim-rs .vim-mode');
  var pos = fig.querySelector('.vim-rs .vim-st .r');
  var mut = fig.querySelector('.vmut');
  var signs = [].slice.call(fig.querySelectorAll('.vsign'));
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;  // the static frame stands

  var visible = false;
  if ('IntersectionObserver' in window) {
    new IntersectionObserver(function (es) { visible = es[0].isIntersecting; }, { threshold: 0.3 }).observe(fig);
  } else visible = true;
  function sleep(ms) {
    return new Promise(function (done) {
      var left = ms, last = performance.now();
      (function tick() {
        var now = performance.now();
        if (visible && !document.hidden) left -= now - last;
        last = now;
        if (left <= 0) done(); else setTimeout(tick, Math.min(left, 60));
      })();
    });
  }
  function esc(s) { return s.replace(/[&<>]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]; }); }
  function line(rs) {
    var d = document.createElement('div');
    d.innerHTML = rs.map(function (r) { return '<span class="t' + r[0] + '">' + esc(r[1]) + '</span>'; }).join('');
    out.appendChild(d);
    while (out.children.length > 5) out.removeChild(out.firstChild);
  }
  async function type(text) {
    cmd.textContent = '';
    for (var i = 1; i <= text.length; i++) {
      cmd.textContent = text.slice(0, i);
      await sleep(30 + Math.random() * 40);
    }
    await sleep(350);
  }
  async function lines(ls, gap) {
    for (var i = 0; i < ls.length; i++) { line(ls[i]); await sleep(gap || 90); }
  }
  function sign(t) { signs.forEach(function (s) { s.textContent = t; }); }
  function setMode(t, cls) { mode.textContent = t; mode.className = 'vim-mode' + (cls ? ' ' + cls : ''); }

  var orig = mut.innerHTML;
  async function loop() {
    for (;;) {
      out.innerHTML = ''; cmd.textContent = ''; sign('');
      fig.classList.remove('pair', 'mutant', 'broken'); setMode('NORMAL');
      await sleep(900);
      // one rule, written twice
      setMode('VISUAL'); pos.textContent = 'rust · 153:36';
      fig.classList.add('pair');
      cmd.innerHTML = '<span class="td">-- the author, or a moderator: once in the code, once in the rule</span>';
      await sleep(3200);
      fig.classList.remove('pair'); setMode('NORMAL'); pos.textContent = 'rust · 153:16';
      // the proofs go through
      await type(':!h5i app prove');
      await lines([[['d', 'extract  kernel → proofs/generated/   charon, aeneas']],
                   [['d', 'lake     build Theorems']]], 260);
      await sleep(900);
      sign('✓');
      await lines([[['o', 'authorized          '], ['g', 'proven']],
                   [['o', 'inv_preserved       '], ['g', 'proven']],
                   [['o', 'transition_total    '], ['g', 'proven']]], 160);
      await sleep(2600);
      // a mutant must break one of them
      await type(':!h5i app mutate');
      out.innerHTML = '';
      setMode('MUTANT', 'red');
      mut.textContent = mut.getAttribute('data-alt');
      fig.classList.add('mutant');
      await lines([[['o', 'mutant   '], ['k', 'p.author == user'], ['d', '  →  '], ['r', 'p.author != user']]]);
      await sleep(900);
      await lines([[['d', 'lake     build Theorems']]]);
      await sleep(1100);
      sign('✗'); fig.classList.add('broken');
      await lines([[['o', 'authorized          '], ['r', 'fails'], ['d', '   a non-author could delete']],
                   [['o', 'mutant   '], ['g', 'caught']]], 400);
      await sleep(1600);
      mut.innerHTML = orig; fig.classList.remove('mutant', 'broken'); sign('✓'); setMode('NORMAL');
      await lines([[['k', '11 of 11 mutants caught']]]);
      cmd.textContent = '';
      await sleep(5000);
    }
  }
  loop();
})();
