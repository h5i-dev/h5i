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

  var SHELL_KW = /^(?:if|then|else|elif|fi|for|while|until|do|done|case|esac|in|function|return|export|local|source|set|unset|echo|cd|sudo)$/;

  // ── bash / shell session ────────────────────────────────
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
      t: function (w, c) { return c.cmd ? (w === 'h5i' || SHELL_KW.test(w) ? 'builtin' : 'cmd') : 'plain'; },
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

  function run() {
    // language-tagged code blocks (the Manual, any markdown-rendered page)
    var coded = document.querySelectorAll('pre > code[class*="language-"]');
    for (var i = 0; i < coded.length; i++) {
      var m = /language-([\w-]+)/.exec(coded[i].className);
      highlight(coded[i], m ? m[1].toLowerCase() : null);
    }
    // plain terminal-session blocks (skip hand-authored ones that already have spans)
    var terms = document.querySelectorAll('.terminal-body pre');
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
