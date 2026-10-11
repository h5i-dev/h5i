(() => {
  const slides = [...document.querySelectorAll('.slide')];
  const prev = document.getElementById('prev');
  const next = document.getElementById('next');
  const position = document.getElementById('position');
  let current = 0;
  const indexFromHash = () => {
    const match = location.hash.match(/^#(?:slide-)?(\d+)$/);
    return match ? Math.max(0, Math.min(slides.length - 1, Number(match[1]) - 1)) : 0;
  };
  function show(index, updateHash = true) {
    current = Math.max(0, Math.min(slides.length - 1, index));
    slides.forEach((slide, i) => { slide.hidden = i !== current; slide.inert = i !== current; });
    position.textContent = `${String(current + 1).padStart(2, '0')} / ${slides.length}`;
    prev.disabled = current === 0;
    next.disabled = current === slides.length - 1;
    if (updateHash) history.replaceState(null, '', `#slide-${current + 1}`);
    window.scrollTo(0, 0);
  }
  function fit() {
    document.documentElement.style.setProperty('--ui-scale', Math.max(1, innerWidth - 48) / 1440);
    document.documentElement.style.setProperty('--deck-scale', Math.min(innerWidth / 1280, Math.max(1, innerHeight - 64) / 720));
  }
  prev.addEventListener('click', () => show(current - 1));
  next.addEventListener('click', () => show(current + 1));
  document.getElementById('print').addEventListener('click', () => window.print());
  window.addEventListener('resize', fit);
  window.addEventListener('hashchange', () => show(indexFromHash(), false));
  window.addEventListener('keydown', event => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey || event.target.closest('a,button,input,textarea,select,[contenteditable]')) return;
    const actions = { ArrowRight:current + 1, PageDown:current + 1, ' ':current + 1, ArrowLeft:current - 1, PageUp:current - 1, Home:0, End:slides.length - 1 };
    if (event.key in actions) { event.preventDefault(); show(actions[event.key]); }
  });
  // Keep every slide readable to PDF/print accessibility tools.
  window.addEventListener('beforeprint', () => slides.forEach(slide => { slide.inert = false; }));
  window.addEventListener('afterprint', () => show(current, false));
  fit();
  show(indexFromHash(), false);
  document.documentElement.classList.add('enhanced');
})();
