/* h5i manual: which chapter and section is being read, and the copy cells.
 *
 * One scroll handler drives both side columns: the left marks the chapter,
 * the right swaps in that chapter's sections and marks the one in view.
 */
(function () {
  var nav = [].slice.call(document.querySelectorAll('.manual-nav a[href^="#"]'));
  var toc = document.querySelector('.ma-toc');
  var secs = [].slice.call(document.querySelectorAll('.ma-sec'));
  var heads = [].slice.call(document.querySelectorAll('.manual-body h2[id], .manual-body h3[id]'));
  if (toc) toc.classList.add('live');

  var chapter = null, section = null;
  function spy() {
    var h2 = null, h3 = null;
    for (var i = 0; i < heads.length; i++) {
      if (heads[i].getBoundingClientRect().top > 110) break;
      if (heads[i].tagName === 'H2') { h2 = heads[i]; h3 = null; } else { h3 = heads[i]; }
    }
    var c = h2 ? h2.id : (heads[0] && heads[0].id);
    if (c !== chapter) {
      chapter = c;
      nav.forEach(function (a) {
        var on = a.getAttribute('href') === '#' + c;
        a.classList.toggle('active', on);
        if (on) a.scrollIntoView({ block: 'nearest' });
      });
      secs.forEach(function (s) { s.classList.toggle('current', s.dataset.sec === c); });
    }
    var s = h3 ? h3.id : c;
    if (s !== section) {
      section = s;
      [].forEach.call(document.querySelectorAll('.ma-toc a'), function (a) {
        a.classList.toggle('active', a.getAttribute('href') === '#' + s);
      });
    }
  }
  var queued = false;
  window.addEventListener('scroll', function () {
    if (queued) return;
    queued = true;
    requestAnimationFrame(function () { queued = false; spy(); });
  }, { passive: true });
  window.addEventListener('resize', spy, { passive: true });
  spy();

  [].forEach.call(document.querySelectorAll('.codeblock .copy-btn'), function (btn) {
    var code = btn.closest('.codeblock').querySelector('pre');
    btn.addEventListener('click', function () {
      navigator.clipboard.writeText(code.textContent.replace(/\n$/, '')).then(function () {
        btn.textContent = 'Copied';
        btn.classList.add('done');
        setTimeout(function () { btn.textContent = 'Copy'; btn.classList.remove('done'); }, 1600);
      });
    });
  });
})();
