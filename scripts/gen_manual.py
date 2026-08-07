#!/usr/bin/env python3
"""Generate docs/manual/index.html from MANUAL.md.

The /manual/ page is RENDERED OUTPUT, not hand-edited. Edit MANUAL.md, then
regenerate:

    pip install markdown        # one-time dependency (python-markdown)
    python3 scripts/gen_manual.py   # run from the repo root

It wraps the rendered manual in the site shell (nav, footer, dark/red theme)
with a sticky sidebar TOC + scrollspy, and uses a GitHub-compatible heading
slugify so MANUAL.md's in-doc cross-references resolve.
"""
import re, markdown

src = open("MANUAL.md", encoding="utf-8").read()

# Drop the hand-written "## Table of Contents" block (we render a styled sidebar instead)
src = re.sub(r'\n## Table of Contents\n.*?(?=\n## )', '\n', src, count=1, flags=re.S)

import re as _re
def gh_slug(value, sep):
    v=value.strip().lower()
    v=_re.sub(r'[\u2000-\u206f\u2e00-\u2e7f\\\'!"#$%&()*+,./:;<=>?@\[\]^`{|}~]','',v)
    return v.replace(' ', sep)
md = markdown.Markdown(extensions=["fenced_code","tables","toc","sane_lists","attr_list"],
                       extension_configs={"toc":{"permalink":False,"slugify":gh_slug,"separator":"-"}})
body = md.convert(src)

# Build sidebar TOC from the heading tokens (levels 2 and 3)
def render_toc(tokens):
    out = []
    for t in tokens:
        if t["level"] == 2:
            out.append(f'<a class="t2" href="#{t["id"]}">{t["name"]}</a>')
            kids = [c for c in t.get("children", []) if c["level"] == 3]
            if kids:
                out.append('<div class="t3group">')
                for c in kids:
                    out.append(f'<a class="t3" href="#{c["id"]}">{c["name"]}</a>')
                out.append('</div>')
        out.extend(render_toc(t.get("children", [])) if t["level"] < 2 else [])
    return out
toc_html = "\n".join(render_toc(md.toc_tokens))

HEAD = '''<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>h5i Manual: CLI Reference for Auditable Agent Workspaces</title>
  <meta name="description" content="The complete h5i CLI reference: every command for sandboxed agent workspaces (h5i env), prompt-aware commits, compressed logs, agent handoffs, audit, and review-ready PR evidence.">
  <meta name="keywords" content="h5i manual, h5i cli reference, h5i env, h5i capture, h5i recall, h5i msg, h5i audit, h5i share, auditable workspace, claude code, codex">
  <meta name="author" content="h5i-dev">
  <meta name="theme-color" content="#D21C1C">
  <meta name="color-scheme" content="dark">
  <meta name="robots" content="index, follow, max-image-preview:large">
  <link rel="canonical" href="https://h5i.dev/manual/">
  <link rel="sitemap" type="application/xml" href="/sitemap.xml">
  <link rel="icon" type="image/png" href="/_static/logo.png">
  <link rel="apple-touch-icon" href="/_static/logo.png">
  <meta property="og:type" content="article">
  <meta property="og:site_name" content="h5i">
  <meta property="og:title" content="h5i Manual: CLI Reference for Auditable Agent Workspaces">
  <meta property="og:description" content="The complete h5i CLI reference: every command for sandboxed agent workspaces, prompt-aware commits, compressed logs, agent handoffs, audit, and review-ready PR evidence.">
  <meta property="og:url" content="https://h5i.dev/manual/">
  <meta property="og:image" content="https://h5i.dev/_static/screenshot_h5i_server.png">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="h5i Manual: CLI Reference for Auditable Agent Workspaces">
  <meta name="twitter:description" content="The complete h5i CLI reference for auditable agent workspaces.">
  <meta name="twitter:image" content="https://h5i.dev/_static/screenshot_h5i_server.png">

  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Archivo:wght@700;800;900&family=Space+Grotesk:wght@300;400;500;700&family=Space+Mono:wght@400;700&display=swap" rel="stylesheet" media="print" onload="this.media='all'">
  <noscript><link href="https://fonts.googleapis.com/css2?family=Archivo:wght@700;800;900&family=Space+Grotesk:wght@300;400;500;700&family=Space+Mono:wght@400;700&display=swap" rel="stylesheet"></noscript>
__STYLE__
</head>
<body>
__NAV__
<main class="manual-wrap">
  <aside class="manual-toc" aria-label="Manual contents">
    <div class="toc-head">CLI Reference</div>
    <nav>
__TOC__
    </nav>
  </aside>
  <article class="manual-body">
__BODY__
  </article>
</main>
__FOOTER__
__SCRIPT__
</body>
</html>
'''

STYLE = '''  <link rel="stylesheet" href="/_static/blog.css">
  <style>
    /* The manual is the site's article layout with a two-level index: the
       measure sits beside a sticky section list, both hung off the sheet's
       own gutter. Everything else comes from the chassis. */
    .manual-wrap{
      display:grid;grid-template-columns:250px minmax(0,var(--measure));
      gap:60px;justify-content:center;align-items:start;
      padding:54px var(--gut) 78px;border-bottom:1px solid var(--line2);
    }
    .manual-toc{position:sticky;top:84px;align-self:start;
      max-height:calc(100vh - 120px);overflow-y:auto;min-width:0;}
    .manual-toc .toc-head{margin:0 0 12px;font-family:var(--mono);font-size:10px;
      letter-spacing:.2em;text-transform:uppercase;color:var(--faint);}
    .manual-toc nav{display:flex;flex-direction:column;}
    .manual-toc a{display:block;padding:6px 0 6px 13px;
      border-left:1px solid var(--line2);color:var(--dim);
      font-size:13px;line-height:1.45;transition:color .15s,border-color .15s;}
    .manual-toc a:hover{color:var(--ink);}
    .manual-toc a.t2{font-family:var(--mono);font-size:12px;color:var(--text);}
    .manual-toc a.t3{padding-left:26px;font-size:12.5px;}
    .manual-toc a.active{color:var(--spot);border-left-color:var(--spot);}
    .manual-toc .t3group{display:flex;flex-direction:column;}
    .manual-toc::-webkit-scrollbar{width:6px;}

    .manual-body{min-width:0;max-width:var(--measure);font-size:16px;color:var(--dim);
      overflow-wrap:break-word;}
    .manual-body h1{font-family:var(--disp);font-weight:900;
      font-size:clamp(32px,4vw,58px);letter-spacing:-.05em;line-height:.95;
      color:var(--ink);margin:0 0 22px;}
    .manual-body h2{font-family:var(--disp);font-weight:800;font-size:25px;
      letter-spacing:-.035em;line-height:1.15;color:var(--ink);
      margin:46px 0 16px;padding-top:18px;border-top:1px solid var(--line);
      scroll-margin-top:84px;}
    .manual-body hr+h2{border-top:0;padding-top:0;margin-top:26px;}
    .manual-body h3{font-family:var(--disp);font-weight:700;font-size:18px;
      letter-spacing:-.02em;color:var(--ink);margin:32px 0 12px;scroll-margin-top:84px;}
    .manual-body h4{font-family:var(--disp);font-weight:700;font-size:15px;
      color:var(--text);margin:24px 0 8px;scroll-margin-top:84px;}
    .manual-body h3 code{font-size:1em;background:none;border:0;padding:0;color:var(--ink);}
    .manual-body p{margin:0 0 18px;}
    .manual-body ul,.manual-body ol{margin:0 0 18px;padding-left:22px;}
    .manual-body li{margin:6px 0;}
    .manual-body li::marker{color:var(--faint);}
    .manual-body strong{color:var(--ink);font-weight:500;}
    .manual-body a{color:var(--spot);text-decoration:underline;
      text-decoration-color:color-mix(in srgb,var(--spot) 45%,transparent);
      text-underline-offset:3px;}
    .manual-body a:hover{text-decoration-color:currentColor;}
    .manual-body a code{color:var(--spot);}
    .manual-body code{font-family:var(--mono);font-size:13.5px;color:var(--ink);
      background:var(--panel);border:1px solid var(--line2);padding:1px 6px;}
    .manual-body pre{background:var(--panel);border:1px solid var(--line2);
      padding:16px 18px;margin:0 0 22px;overflow-x:auto;}
    .manual-body pre code{background:none;border:0;padding:0;
      font-size:13.5px;line-height:1.7;color:var(--text);white-space:pre;}
    .manual-body blockquote{margin:26px 0;padding-left:18px;
      border-left:2px solid var(--spot-solid);color:var(--ink);}
    .manual-body blockquote p{color:var(--ink);margin:0 0 8px;}
    .manual-body hr{border:0;border-top:1px solid var(--line);margin:34px 0;}
    .manual-body table{width:100%;border-collapse:collapse;margin:0 0 22px;
      font-family:var(--mono);font-size:12.5px;display:block;overflow-x:auto;}
    .manual-body th,.manual-body td{border:1px solid var(--line);
      padding:10px 13px;text-align:left;vertical-align:top;font-weight:400;}
    .manual-body th{background:var(--panel);color:var(--faint);
      font-size:10px;letter-spacing:.16em;text-transform:uppercase;white-space:nowrap;}
    .manual-body td{color:var(--dim);}

    @media (max-width:1100px){
      .manual-wrap{grid-template-columns:minmax(0,1fr);gap:0;}
      .manual-toc{display:none;}
    }
  </style>'''

NAV = '''<nav>
  <a class="nav-logo" href="/">
    <img src="/_static/logo.png" alt="h5i">
    <span>h5i</span>
  </a>
  <ul class="nav-links">
    <li><a href="/features/">Features</a></li>
    <li><a href="/guides/">Guides</a></li>
    <li><a href="/manual/">Manual</a></li>
    <li><a href="/blog/">Blog</a></li>
    <li><a href="https://github.com/h5i-dev/h5i" class="nav-cta">GitHub &rarr;</a></li>
  </ul>
</nav>'''

FOOTER = '''<footer>
  <div class="footer-inner">
    <div class="footer-brand">
      <img src="/_static/logo.png" alt="h5i">
      <span>h5i<span class="red"> / high-five</span></span>
    </div>
    <nav class="footer-links">
      <a href="https://github.com/h5i-dev/h5i">GitHub</a>
      <a href="/features/">Features</a>
      <a href="/guides/">Guides</a>
      <a href="/workflows/">Workflows</a>
      <a href="/manual/">Manual</a>
      <a href="https://github.com/h5i-dev/h5i/issues">Issues</a>
      <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE">License</a>
    </nav>
    <div class="footer-legal">Apache 2.0 · Built with Rust</div>
  </div>
</footer>'''

SCRIPT = '''<script src="/_static/blog.js" defer></script>
<script src="/_static/highlight.js" defer></script>
<script>
  // scrollspy: highlight the current section in the sidebar
  (function(){
    var links=[].slice.call(document.querySelectorAll('.manual-toc a'));
    var map={};links.forEach(function(a){map[a.getAttribute('href').slice(1)]=a;});
    var heads=[].slice.call(document.querySelectorAll('.manual-body h2[id],.manual-body h3[id]'));
    var obs=new IntersectionObserver(function(es){
      es.forEach(function(e){
        if(e.isIntersecting){
          links.forEach(function(a){a.classList.remove('active');});
          var a=map[e.target.id];if(a){a.classList.add('active');a.scrollIntoView({block:'nearest'});}
        }
      });
    },{rootMargin:'-80px 0px -70% 0px'});
    heads.forEach(function(h){obs.observe(h);});
  })();
</script>'''

page = (HEAD.replace("__STYLE__", STYLE).replace("__NAV__", NAV)
        .replace("__TOC__", toc_html).replace("__BODY__", body)
        .replace("__FOOTER__", FOOTER).replace("__SCRIPT__", SCRIPT))
open("docs/manual/index.html", "w", encoding="utf-8").write(page)
print("wrote docs/manual/index.html  bytes:", len(page))
print("h2 sections:", body.count("<h2"), "| h3:", body.count("<h3"), "| code blocks:", body.count("<pre"), "| tables:", body.count("<table"))
