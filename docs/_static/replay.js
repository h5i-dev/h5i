/* h5i landing: a terminal that replays one engagement.
 *
 * The page says what h5i is by running it: an agent opens a session, reads the
 * page, replays a request with one field changed and records what it found,
 * then a kernel's proofs are checked. Commands are typed with the odd slip and
 * correction; output lands a line at a time. It stops when off screen, when the
 * tab is hidden or when paused, and with reduced motion it shows the last frame.
 */
(function () {
  var root = document.querySelector('.tty');
  if (!root) return;
  var screen = root.querySelector('.tty-screen');
  var label = root.querySelector('.tty-label');
  var mode = root.querySelector('.tty-mode');
  var where = root.querySelector('.tty-where');
  var right = root.querySelector('.tty-right');
  var toggle = root.querySelector('.tty-toggle');

  // [class, text] runs. o = output, d = faint, k = ink, r = red, g = green.
  var ACTS = [
    { label: 'agent@box: ~/shop-audit', where: 'checkout-idor', count: 205, steps: [
      { mode: 'browser', cmd: 'h5i browser open https://shop.example.test --capture --session checkout-idor', spin: 900, req: 2,
        out: [[['o', '  '], ['k', 'checkout-idor'], ['d', '  live · engine h5i · capture on']]] },
      { mode: 'browser', cmd: 'h5i browser snapshot', note: '# the page as an outline',
        out: [[['o', '  heading "Your orders"']],
              [['o', '  link "Order #1041"          '], ['k', '@e7']],
              [['o', '  button "Pay now"            '], ['k', '@e9']]] },
      { mode: 'browser', cmd: 'h5i browser click @e7', req: 3,
        out: [[['o', '  → /checkout?order=1041  '], ['d', '200 · 3 requests']]] },
      { mode: 'websec', cmd: 'h5i websec requests --human', note: '# what the session sent',
        out: [[['o', '  #107  GET  /checkout?order=1041     200  2.1 kB']],
              [['o', '  #108  GET  /api/order/1041          200  1.4 kB']],
              [['o', '  #112  GET  ps.example.invalid/t.js  '], ['r', 'refused'], ['d', '  out of scope']]] },
      { mode: 'websec', cmd: 'h5i websec replay req_108 --set path=/api/order/1042', spin: 600, req: 1,
        out: [[['o', '  res_109  200  1.9 kB']]] },
      { mode: 'websec', cmd: 'h5i websec diff res_108 res_109 --human',
        out: [[['o', '  status : 200 → 200']],
              [['o', '  bytes  : 1412 → 1903 (+491)']],
              [['o', '  owner  : u_17 → '], ['r', 'u_23'], ['d', '   another customer\'s order']]] },
      { mode: 'finding', cmd: 'h5i websec finding create --title "cross-user order read" --evidence req_108,res_109',
        out: [[['o', '  '], ['k', 'finding_1'], ['o', '  recorded · cites req_108, res_109']]] }
    ] },
    { label: 'agent@box: ~/board', where: 'examples/app/board', status: 'lean 4 · 3 theorems', steps: [
      { mode: 'app', cmd: 'h5i app prove', note: '# extract, build, then the gate', spin: 1600,
        out: [[['o', '  extract  kernel → proofs/generated/']],
              [['o', '  lake     build Theorems   '], ['d', 'ok']],
              [['o', '  authorized          '], ['g', 'proven']],
              [['o', '  inv_preserved       '], ['g', 'proven']],
              [['o', '  transition_total    '], ['g', 'proven']]] },
      { mode: 'app', cmd: 'h5i app mutate', note: '# each mutant must break a proof', spin: 1400,
        out: [[['o', '  drop the moderator check   caught  '], ['d', 'authorized']],
              [['o', '  skip the state write       caught  '], ['d', 'inv_preserved']],
              [['d', '  … 9 more']],
              [['k', '  11 of 11 mutants caught']]] }
    ] }
  ];

  var ROWS = 40;  // lines kept; the screen shows as many as fit
  var NEAR = {};
  ['qwertyuiop', 'asdfghjkl', 'zxcvbnm'].forEach(function (row) {
    for (var i = 0; i < row.length; i++) NEAR[row[i]] = (row[i - 1] || '') + (row[i + 1] || '');
  });

  var lines = [], cur = null, act = ACTS[0], count = 0;
  var paused = false, visible = true, fast = false;

  function esc(s) { return s.replace(/[&<>]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]; }); }
  function runs(rs) {
    return rs.map(function (r) { return '<span class="t' + r[0] + '">' + esc(r[1]) + '</span>'; }).join('');
  }
  // a typed command, coloured as far as it has been typed
  function cmdRuns(text, note) {
    var out = [['p', '$ ']], words = text.split(/( )/);
    words.forEach(function (w, i) {
      var k = i < 3 ? 'c' : /^--/.test(w) ? 'f' : 'a';
      out.push([k, w]);
    });
    if (note) out.push(['m', '   ' + note]);
    return out;
  }
  function draw() {
    var html = lines.slice(-ROWS).map(function (l) { return '<div>' + (runs(l) || ' ') + '</div>'; });
    if (cur) html.push('<div>' + runs(cur) + '<span class="cur"></span></div>');
    screen.innerHTML = html.join('');
    screen.scrollTop = screen.scrollHeight;  // fills from the top, then scrolls like a shell
  }
  function status(m) {
    if (m) mode.textContent = m;
    where.textContent = act.where;
    right.textContent = act.status || (count + ' recorded');
  }

  function halted() { return paused || !visible || document.hidden; }
  function sleep(ms) {
    if (fast) return Promise.resolve();
    return new Promise(function (done) {
      var left = ms, last = performance.now();
      (function tick() {
        var now = performance.now();
        if (!halted()) left -= now - last;
        last = now;
        if (left <= 0) done(); else setTimeout(tick, Math.min(left, 60));
      })();
    });
  }
  function rand(a, b) { return a + Math.random() * (b - a); }

  async function type(cmd, note) {
    root.classList.add('typing');
    var slip = -1;
    if (Math.random() < 0.55) {
      var i = Math.floor(rand(8, cmd.length - 3));
      if (NEAR[cmd[i]]) slip = i;
    }
    for (var i = 0; i <= cmd.length; i++) {
      if (i === slip) {
        var near = NEAR[cmd[i]], wrong = near[Math.floor(Math.random() * near.length)];
        cur = cmdRuns(cmd.slice(0, i) + wrong); draw();
        await sleep(rand(260, 420));
        cur = cmdRuns(cmd.slice(0, i)); draw();
        await sleep(rand(90, 160));
      }
      cur = cmdRuns(cmd.slice(0, i)); draw();
      await sleep(cmd[i - 1] === ' ' ? rand(70, 140) : rand(22, 58));
    }
    if (note) {
      await sleep(rand(160, 280));
      cur = cmdRuns(cmd, note); draw();
    }
    root.classList.remove('typing');
    await sleep(rand(260, 420));
  }

  var SPIN = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
  async function run(step) {
    status(step.mode);
    await type(step.cmd, step.note);
    lines.push(cur); cur = null; draw();
    if (step.spin && !fast) {
      for (var t = 0, f = 0; t < step.spin; t += 80, f++) {
        cur = [['d', '  ' + SPIN[f % SPIN.length]]]; draw();
        await sleep(80);
      }
      cur = null;
    }
    count += step.req || 0; status();
    for (var j = 0; j < step.out.length; j++) {
      lines.push(step.out[j]); draw();
      await sleep(rand(50, 110));
    }
    cur = [['p', '$ ']]; draw();
    await sleep(rand(900, 1400));
  }

  async function play(a) {
    act = a; count = a.count || 0;
    label.textContent = a.label;
    status();
    for (var i = 0; i < a.steps.length; i++) await run(a.steps[i]);
  }

  async function loop() {
    for (;;) {
      for (var i = 0; i < ACTS.length; i++) {
        if (i) {
          await type('clear');
          lines = []; cur = [['p', '$ ']]; draw();
          await sleep(500);
        }
        await play(ACTS[i]);
        await sleep(i === ACTS.length - 1 ? 6000 : 3500);
      }
      await type('clear');
      lines = []; cur = [['p', '$ ']]; draw();
      await sleep(600);
    }
  }

  toggle.addEventListener('click', function () {
    paused = !paused;
    root.classList.toggle('paused', paused);
    toggle.textContent = paused ? 'Play' : 'Pause';
    toggle.setAttribute('aria-pressed', paused ? 'true' : 'false');
  });
  if ('IntersectionObserver' in window) {
    new IntersectionObserver(function (es) { visible = es[0].isIntersecting; }).observe(root);
  }

  lines = []; cur = [['p', '$ ']]; draw();
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
    // the whole first act, typed out at once, and nothing moves
    fast = true;
    toggle.hidden = true;
    play(ACTS[0]);
    return;
  }
  loop();
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
