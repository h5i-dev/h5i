/* ════════════════════════════════════════════════════════
   h5i — tiny self-hosted syntax highlighter (no dependencies)

   Targets:
     • <pre><code class="language-bash|yaml|json|toml|ini|…">  (the Manual)
     • .terminal-body <pre>  that contain ONLY text  (plain session blocks)

   Hand-authored terminal blocks (already wrapped in <span class="t-…">)
   are left untouched — we skip any <pre> that already has element children.

   Technique: an ordered list of sticky (/…/y) rules scanned left→right.
   At each position the first matching rule wins, emits one token, and we
   advance past it. Anything unmatched is emitted as a single escaped char.
   Because the scan only ever moves forward over already-decoded text and
   re-escapes on output, it cannot corrupt markup.
   ════════════════════════════════════════════════════════ */
(function () {
  'use strict';

  var ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;' };
  function esc(s) { return s.replace(/[&<>]/g, function (c) { return ESC[c]; }); }

  function render(tokens) {
    var out = '';
    for (var i = 0; i < tokens.length; i++) {
      var t = tokens[i];
      out += t.t === 'plain'
        ? esc(t.v)
        : '<span class="hl-' + t.t + '">' + esc(t.v) + '</span>';
    }
    return out;
  }

  // Generic forward scanner. `rules` is an ordered array of:
  //   { t, re, when?(ctx), after?(text, ctx) }
  // `t` may be a string or a function(text, ctx) -> string.
  function lex(src, rules) {
    var tokens = [], pos = 0, n = src.length;
    var ctx = { lineStart: true, ws: true, cmd: true };
    while (pos < n) {
      var hit = false;
      for (var r = 0; r < rules.length; r++) {
        var rule = rules[r];
        if (rule.when && !rule.when(ctx)) continue;
        rule.re.lastIndex = pos;
        var m = rule.re.exec(src);
        if (m && m.index === pos && m[0].length) {
          var text = m[0];
          var type = typeof rule.t === 'function' ? rule.t(text, ctx) : rule.t;
          tokens.push({ t: type, v: text });
          if (rule.after) rule.after(text, ctx);
          // central line/word-boundary bookkeeping
          ctx.ws = /\s$/.test(text);
          if (text.indexOf('\n') >= 0) ctx.lineStart = true;
          else if (!/^[ \t]+$/.test(text)) ctx.lineStart = false;
          pos += text.length;
          hit = true;
          break;
        }
      }
      if (!hit) {
        var ch = src.charAt(pos);
        tokens.push({ t: 'plain', v: ch });
        ctx.ws = /\s/.test(ch);
        ctx.lineStart = ch === '\n';
        ctx.cmd = ch === '\n';
        pos++;
      }
    }
    return tokens;
  }

  function lexer(rules) { return function (src) { return lex(src, rules); }; }

  // ── bash / shell session ────────────────────────────────
  // The command word (incl. `h5i`) is rendered uniformly (hl-cmd); the red
  // `$` prompt is the single accent, matching the site's terminal style.
  var BASH = lexer([
    { t: 'plain', re: /\n/y, after: function (_, c) { c.cmd = true; } },
    { t: 'plain', re: /[ \t]+/y },
    { t: 'prompt', re: /\$(?= )/y, when: function (c) { return c.lineStart; }, after: function (_, c) { c.cmd = true; } },
    { t: 'comment', re: /#[^\n]*/y, when: function (c) { return c.ws; } },
    { t: 'string', re: /"(?:\\.|[^"\\\n])*"/y },
    { t: 'string', re: /'[^'\n]*'/y },
    { t: 'var', re: /\$\{[^}\n]*\}|\$\w+/y },
    { t: 'flag', re: /--?[A-Za-z][\w-]*/y, after: function (_, c) { c.cmd = false; } },
    { t: 'op', re: /&&|\|\||[;|]/y, after: function (_, c) { c.cmd = true; } },
    { t: 'op', re: /[<>&]/y },
    {
      t: function (w, c) { return c.cmd ? 'cmd' : 'plain'; },
      re: /[\w./@:+=~-]+/y,
      after: function (_, c) { c.cmd = false; }
    }
  ]);

  // ── yaml ────────────────────────────────────────────────
  var YAML = lexer([
    { t: 'plain', re: /\n/y },
    { t: 'plain', re: /[ \t]+/y },
    { t: 'comment', re: /#[^\n]*/y, when: function (c) { return c.ws; } },
    { t: 'op', re: /-(?= )/y, when: function (c) { return c.lineStart; } },
    { t: 'key', re: /[A-Za-z0-9_.$-]+(?=\s*:)/y, when: function (c) { return c.ws; } },
    { t: 'string', re: /"(?:\\.|[^"\\\n])*"|'[^'\n]*'/y },
    { t: 'bool', re: /\b(?:true|false|null|yes|no|on|off)\b/y },
    { t: 'num', re: /-?\b\d+(?:\.\d+)?\b/y },
    { t: 'punct', re: /[:\[\]{},]/y },
    { t: 'plain', re: /[^\s:#,\[\]{}]+/y }
  ]);

  // ── json ────────────────────────────────────────────────
  var JSON_L = lexer([
    { t: 'plain', re: /\s+/y },
    { t: 'key', re: /"(?:\\.|[^"\\\n])*"(?=\s*:)/y },
    { t: 'string', re: /"(?:\\.|[^"\\\n])*"/y },
    { t: 'bool', re: /\b(?:true|false|null)\b/y },
    { t: 'num', re: /-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/y },
    { t: 'punct', re: /[{}\[\],:]/y }
  ]);

  // ── toml / ini ──────────────────────────────────────────
  var TOML = lexer([
    { t: 'plain', re: /\n/y },
    { t: 'plain', re: /[ \t]+/y },
    { t: 'comment', re: /[#;][^\n]*/y, when: function (c) { return c.ws; } },
    { t: 'section', re: /\[[^\]\n]*\]/y, when: function (c) { return c.lineStart; } },
    { t: 'key', re: /[A-Za-z0-9_.-]+(?=\s*=)/y, when: function (c) { return c.ws; } },
    { t: 'string', re: /"(?:\\.|[^"\\\n])*"|'[^'\n]*'/y },
    { t: 'bool', re: /\b(?:true|false)\b/y },
    { t: 'num', re: /-?\b\d+(?:\.\d+)?\b/y },
    { t: 'punct', re: /[=,\[\]{}]/y },
    { t: 'plain', re: /[^\s=#;]+/y }
  ]);

  var LANGS = {
    bash: BASH, sh: BASH, shell: BASH, 'shell-session': BASH, console: BASH, zsh: BASH,
    yaml: YAML, yml: YAML,
    json: JSON_L,
    toml: TOML, ini: TOML
  };

  function highlight(el, lang) {
    if (el.dataset.hl) return;
    el.dataset.hl = '1';
    var fn = LANGS[lang];
    if (!fn) return;              // unknown language → leave as-is (already escaped)
    try { el.innerHTML = render(fn(el.textContent)); }
    catch (e) { /* fail open: keep the original text */ }
  }

  // Conservative shell detection for code blocks with NO language hint
  // (e.g. the Manual's bare ```fences```). We only highlight a block when it
  // reads like commands and shows no rendered-output/diagram glyphs — so usage
  // blocks like `h5i recall log [options]` light up while status dumps and box
  // diagrams (✔ Committed…, ── Heatmap ──) are left untouched.
  var OUT_GLYPH = /[─│┌┐└┘├┤┼█░▓▒●○✔✓✗✘⚠✨ℹ]/;
  var SHELL_CMD = /^(?:\$\s*)?(?:h5i|git|cargo|cd|curl|wget|npm|npx|pnpm|yarn|bash|sh|zsh|export|claude|codex|sudo|mkdir|rm|cp|mv|cat|ls|echo|chmod|chown|ssh|scp|docker|podman|pip3?|python3?|node|make|tar|grep|sed|awk|source|set|unset|kill|touch|brew|apt)\b/;
  function looksShell(text) {
    if (OUT_GLYPH.test(text)) return false;
    var lines = text.split('\n'), kept = 0, hits = 0;
    for (var i = 0; i < lines.length; i++) {
      var l = lines[i].replace(/^\s+/, '');
      if (!l) continue;
      kept++;
      if (l.charAt(0) === '#' || SHELL_CMD.test(l)) hits++;   // command or comment line
    }
    return kept > 0 && hits * 2 >= kept;     // majority of non-blank lines look like shell
  }

  function run() {
    // <pre><code> blocks: use the language hint if present, else sniff for shell.
    var coded = document.querySelectorAll('pre > code');
    for (var i = 0; i < coded.length; i++) {
      var code = coded[i];
      var m = /language-([\w-]+)/.exec(code.className || '');
      if (m) highlight(code, m[1].toLowerCase());
      else if (code.childElementCount === 0 && looksShell(code.textContent)) highlight(code, 'bash');
    }
    // terminal-session blocks (skip hand-authored ones that already have spans)
    var terms = document.querySelectorAll('.terminal pre');
    for (var j = 0; j < terms.length; j++) {
      if (terms[j].childElementCount === 0) highlight(terms[j], 'bash');
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', run);
  } else {
    run();
  }
})();
