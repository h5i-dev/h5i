/* h5i — shared client behavior.
 *
 * Everything here is built from markup that is already on the page, so a
 * page gets the skip link, the responsive nav and the section index without
 * carrying any of them in its own source.
 */
(function () {
  function slug(text) {
    return text.toLowerCase()
      .replace(/[^\w\s-]/g, '')
      .trim()
      .replace(/\s+/g, '-')
      .slice(0, 60);
  }

  /* Skip-to-content link — first focusable element, for keyboard and AT. */
  function skipLink() {
    if (document.querySelector('.skip-link')) return;
    var main = document.querySelector('main, article, .article-wrap');
    if (!main) return;
    if (!main.id) main.id = 'main';
    main.setAttribute('tabindex', '-1');
    var skip = document.createElement('a');
    skip.className = 'skip-link';
    skip.href = '#' + main.id;
    skip.textContent = 'Skip to content';
    document.body.insertBefore(skip, document.body.firstChild);
  }

  function scrollProgress() {
    var bar = document.querySelector('.scroll-progress');
    if (!bar) {
      bar = document.createElement('div');
      bar.className = 'scroll-progress';
      document.body.appendChild(bar);
    }
    bar.setAttribute('aria-hidden', 'true');

    function update() {
      var doc = document.documentElement;
      var scrollable = doc.scrollHeight - doc.clientHeight;
      bar.style.width = scrollable > 0
        ? ((window.scrollY / scrollable) * 100) + '%'
        : '0';
    }
    window.addEventListener('scroll', update, { passive: true });
    window.addEventListener('resize', update, { passive: true });
    update();
  }

  /* The narrow-width nav. Below the nav's own breakpoint the links come off
     the bar and drop into a sheet under it — a continuation of the bar rather
     than a modal, so it keeps the same hairline rows. */
  function responsiveNav() {
    var nav = document.querySelector('nav.blog-nav') ||
              document.querySelector('body > nav');
    if (!nav) return;
    var list = nav.querySelector('.nav-links');
    if (!list) return;

    var sheet = document.createElement('div');
    sheet.className = 'nav-sheet';
    sheet.id = 'nav-sheet';
    [].forEach.call(list.querySelectorAll('a'), function (a) {
      var copy = document.createElement('a');
      copy.href = a.getAttribute('href');
      copy.textContent = a.textContent.trim();
      sheet.appendChild(copy);
    });

    var btn = document.createElement('button');
    btn.className = 'nav-menu';
    btn.type = 'button';
    btn.setAttribute('aria-expanded', 'false');
    btn.setAttribute('aria-controls', 'nav-sheet');
    btn.setAttribute('aria-label', 'Menu');
    btn.innerHTML = '<span></span><span></span><span></span>';

    /* the legacy hamburger and its inline handler are replaced wholesale */
    var legacy = nav.querySelector('.nav-hamburger');
    if (legacy) legacy.parentNode.removeChild(legacy);
    nav.appendChild(btn);
    nav.parentNode.insertBefore(sheet, nav.nextSibling);

    function close() {
      sheet.classList.remove('open');
      btn.setAttribute('aria-expanded', 'false');
    }
    btn.addEventListener('click', function () {
      var open = sheet.classList.toggle('open');
      btn.setAttribute('aria-expanded', open ? 'true' : 'false');
    });
    sheet.addEventListener('click', function (e) {
      if (e.target.tagName === 'A') close();
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') close();
    });
    document.addEventListener('click', function (e) {
      if (!nav.contains(e.target) && !sheet.contains(e.target)) close();
    });

    /* mark the entry for the section being read */
    var here = location.pathname.replace(/\/index\.html$/, '/');
    [].forEach.call(nav.querySelectorAll('.nav-links a'), function (a) {
      var href = a.getAttribute('href') || '';
      if (href.charAt(0) !== '/' || href === '/') return;
      if (here.indexOf(href) === 0) a.setAttribute('aria-current', 'page');
    });
  }

  /* A sticky section index beside the measure, built from the article's own
     headings. Short pieces do not get one: three entries is the point at
     which a jump list beats scrolling. */
  function sectionIndex() {
    var wrap = document.querySelector('.article-wrap');
    var post = wrap && wrap.querySelector('article.post');
    if (!post) return;

    var heads = [].filter.call(post.querySelectorAll('h2'), function (h) {
      return !h.closest('.post-cta, .next-up') && h.textContent.trim();
    });
    if (heads.length < 3) return;

    var aside = document.createElement('aside');
    aside.className = 'toc';
    aside.setAttribute('aria-label', 'Sections');
    var head = document.createElement('p');
    head.className = 'toc-head';
    head.textContent = 'Sections';
    aside.appendChild(head);
    var list = document.createElement('nav');
    aside.appendChild(list);

    var links = heads.map(function (h, i) {
      if (!h.id) h.id = slug(h.textContent) || ('section-' + (i + 1));
      var a = document.createElement('a');
      a.href = '#' + h.id;
      a.textContent = h.textContent.trim();
      list.appendChild(a);
      return a;
    });

    wrap.appendChild(aside);
    wrap.classList.add('has-toc');

    /* scroll-spy: the last heading whose top has passed under the nav */
    var current = null;
    function spy() {
      var pick = links[0];
      for (var i = 0; i < heads.length; i++) {
        if (heads[i].getBoundingClientRect().top <= 96) pick = links[i];
      }
      if (pick === current) return;
      if (current) current.removeAttribute('aria-current');
      pick.setAttribute('aria-current', 'true');
      current = pick;
    }
    window.addEventListener('scroll', spy, { passive: true });
    window.addEventListener('resize', spy, { passive: true });
    spy();
  }

  /* Any command bar with a copy button, wherever it appears. */
  function copyButtons() {
    [].forEach.call(document.querySelectorAll('.hero-install .copy-btn'), function (btn) {
      var code = btn.parentElement.querySelector('code');
      if (!code) return;
      btn.removeAttribute('onclick');
      btn.addEventListener('click', function () {
        navigator.clipboard.writeText(code.textContent.trim()).then(function () {
          var was = btn.textContent;
          btn.textContent = 'copied';
          setTimeout(function () { btn.textContent = was; }, 2000);
        });
      });
    });
  }

  function init() {
    skipLink();
    scrollProgress();
    responsiveNav();
    sectionIndex();
    copyButtons();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
