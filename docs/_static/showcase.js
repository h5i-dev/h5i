/* h5i showcase: pointer spotlight, filter, scroll reveal, terminal replay.
 * The page is complete without this file; every effect only adds motion. */
(function () {
  var root = document.documentElement;
  var main = document.querySelector('main.sc');
  if (!main) return;
  var still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (!still && 'IntersectionObserver' in window) root.classList.add('sc-js');

  // Spotlight: one listener, the hovered cell gets the pointer position.
  main.addEventListener('pointermove', function (e) {
    var cell = e.target.closest && e.target.closest('.sc-card');
    if (!cell) return;
    var r = cell.getBoundingClientRect();
    cell.style.setProperty('--mx', (e.clientX - r.left) + 'px');
    cell.style.setProperty('--my', (e.clientY - r.top) + 'px');
  });

  // Filter chips. The hash keeps a filtered view linkable.
  var chips = [].slice.call(document.querySelectorAll('.sc-chip'));
  function pick(name, scroll) {
    if (!chips.some(function (c) { return c.dataset.filter === name; })) name = 'all';
    main.dataset.filter = name;
    chips.forEach(function (c) { c.setAttribute('aria-pressed', String(c.dataset.filter === name)); });
    reveal();
    if (scroll) {
      var bar = document.querySelector('.sc-filter');
      window.scrollTo({ top: bar.offsetTop - 60 + 1, behavior: still ? 'auto' : 'smooth' });
    }
  }
  chips.forEach(function (c) {
    c.addEventListener('click', function () {
      pick(c.dataset.filter, true);
      try { history.replaceState(null, '', c.dataset.filter === 'all' ? location.pathname : '#' + c.dataset.filter); } catch (_) {}
    });
  });

  // Reveal on scroll, staggered within a row.
  var io = root.classList.contains('sc-js') ? new IntersectionObserver(function (entries) {
    var n = 0;
    entries.forEach(function (en) {
      if (!en.isIntersecting) return;
      en.target.style.transitionDelay = (n++ * 70) + 'ms';
      en.target.classList.add('in');
      io.unobserve(en.target);
    });
  }, { rootMargin: '0px 0px -8% 0px' }) : null;
  function reveal() {
    if (!io) return;
    [].forEach.call(document.querySelectorAll('[data-reveal]:not(.in)'), function (el) { io.observe(el); });
  }

  // Stats count up once.
  function countUp() {
    [].forEach.call(document.querySelectorAll('.sc-stat-n[data-count]'), function (el) {
      var to = +el.dataset.count, t0 = null;
      if (still || !to) return;
      function step(t) {
        if (t0 === null) t0 = t;
        var k = Math.min(1, (t - t0) / 900), v = Math.round(to * (1 - Math.pow(1 - k, 3)));
        el.textContent = (v < 10 ? '0' : '') + v;
        if (k < 1) requestAnimationFrame(step);
      }
      requestAnimationFrame(step);
    });
  }

  // The window: write the Rust kernel, state the theorem, then prove it in
  // the shell. Loops until the reader picks a tab, which stops it.
  var tabs = [].slice.call(document.querySelectorAll('.sc-tab'));
  var panes = [].slice.call(document.querySelectorAll('.sc-term-body'));
  var lang = document.querySelector('.sc-term-lang');
  var run = 0;
  function show(i) {
    tabs.forEach(function (t, k) { t.setAttribute('aria-selected', String(k === i)); });
    panes.forEach(function (p, k) { p.hidden = k !== i; });
    if (lang) lang.textContent = panes[i].dataset.lang;
  }
  function wait(ms, id) {
    return new Promise(function (go, stop) {
      setTimeout(function () { id === run ? go() : stop(); }, ms);
    });
  }
  async function play(id) {
    for (;;) {
      for (var i = 0; i < panes.length; i++) {
        var lines = [].slice.call(panes[i].querySelectorAll('.t-line'));
        lines.forEach(function (l) { l.classList.add('off'); });
        panes[i].scrollTop = 0;
        show(i);
        await wait(400, id);
        for (var k = 0; k < lines.length; k++) {
          var l = lines[k], cmd = l.querySelector('.t-in');
          l.classList.remove('off');
          if (cmd) {
            var text = cmd.dataset.text || (cmd.dataset.text = cmd.textContent);
            for (var c = 0; c <= text.length; c++) { cmd.textContent = text.slice(0, c); await wait(28 + Math.random() * 40, id); }
            await wait(300, id);
          } else {
            await wait(i < 2 ? 70 : 160, id);
          }
          panes[i].scrollTop = panes[i].scrollHeight;
        }
        await wait(i < 2 ? 1600 : 4200, id);
      }
    }
  }
  function settle() {
    run++;
    panes.forEach(function (p) {
      [].forEach.call(p.querySelectorAll('.t-line'), function (l) { l.classList.remove('off'); });
      [].forEach.call(p.querySelectorAll('.t-in[data-text]'), function (c) { c.textContent = c.dataset.text; });
    });
  }
  tabs.forEach(function (t, i) {
    t.addEventListener('click', function () { settle(); show(i); panes[i].scrollTop = 0; });
  });
  function replay() {
    if (!panes.length || !root.classList.contains('sc-js')) return;
    play(++run).catch(function () {});
  }

  window.addEventListener('hashchange', function () {
    var h = location.hash.slice(1);
    if (main.dataset.filter !== 'all' && h && h !== main.dataset.filter) pick('all', false);
  });
  pick(location.hash.slice(1) || 'all', false);
  countUp();
  replay();
})();
