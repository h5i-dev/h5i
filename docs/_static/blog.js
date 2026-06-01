/* h5i blog/docs — shared client behavior.
 * Injects a scroll-progress bar; self-contained, no markup required. */
(function () {
  function init() {
    // Skip-to-content link (first focusable element, for keyboard/AT users).
    var main = document.querySelector('main, article, .article-wrap');
    if (main) {
      if (!main.id) main.id = 'main';
      main.setAttribute('tabindex', '-1');
      var skip = document.createElement('a');
      skip.className = 'skip-link';
      skip.href = '#' + main.id;
      skip.textContent = 'Skip to content';
      document.body.insertBefore(skip, document.body.firstChild);
    }

    var bar = document.createElement('div');
    bar.className = 'scroll-progress';
    bar.setAttribute('aria-hidden', 'true');
    document.body.appendChild(bar);

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

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
