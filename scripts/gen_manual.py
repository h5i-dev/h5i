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
  <link href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@300;400;500;700&family=Space+Mono:wght@400;700&display=swap" rel="stylesheet" media="print" onload="this.media='all'">
  <noscript><link href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@300;400;500;700&family=Space+Mono:wght@400;700&display=swap" rel="stylesheet"></noscript>
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

STYLE = '''  <style>
    :root{
      --red:#e0241b; --red-text:#ff6258; --red-glow:rgba(224,36,27,0.10);
      --bg:#08080b; --bg-card:#0e0e12; --bg-card2:#141419; --bg-term:#0b0b0f;
      --border:rgba(255,255,255,0.09); --border-hi:rgba(255,255,255,0.20);
      --ink:#ffffff; --text:#e8e8ec; --text-dim:#9a9aa4; --text-faint:#6b6b75;
      --green:#62c98c; --cyan:#a9bdd0;
      --mono:'Space Mono',ui-monospace,monospace;
      --sans:'Space Grotesk',-apple-system,system-ui,sans-serif;
    }
    *,*::before,*::after{box-sizing:border-box;margin:0;padding:0;}
    html{scroll-behavior:smooth;-webkit-text-size-adjust:100%;}
    body{background:var(--bg);font-family:var(--sans);color:var(--text);line-height:1.6;
      -webkit-font-smoothing:antialiased;}
    ::selection{background:var(--red);color:#fff;}
    a{color:var(--red-text);text-decoration:none;}
    a:hover{text-decoration:underline;}

    /* nav (mirrors site) */
    nav{position:sticky;top:0;z-index:50;display:flex;align-items:center;justify-content:space-between;
      padding:0 clamp(1rem,4vw,3rem);height:64px;background:rgba(8,8,11,0.86);
      backdrop-filter:blur(12px);border-bottom:1px solid var(--border);}
    .nav-logo{display:flex;align-items:center;gap:0.55rem;font-family:var(--mono);font-weight:700;
      font-size:1.05rem;color:var(--ink);}
    .nav-logo img{width:28px;height:28px;border-radius:6px;}
    .nav-links{display:flex;align-items:center;gap:1.5rem;list-style:none;}
    .nav-links a{color:var(--text-dim);transition:color .18s;font-size:.92rem;}
    .nav-links a:hover{color:var(--ink);text-decoration:none;}
    .nav-links a.nav-cta{background:var(--ink);color:#08080b !important;padding:.45rem .85rem;border-radius:7px;font-weight:500;}
    .nav-links a[aria-current="page"]{color:var(--red-text);}
    .nav-hamburger{display:none;flex-direction:column;gap:5px;cursor:pointer;}
    .nav-hamburger span{width:24px;height:2px;background:var(--ink);}

    /* layout */
    .manual-wrap{display:grid;grid-template-columns:266px minmax(0,1fr);gap:2.5rem;
      max-width:1180px;margin:0 auto;padding:2.4rem clamp(1rem,4vw,3rem) 5rem;}
    .manual-toc{position:sticky;top:84px;align-self:start;max-height:calc(100vh - 104px);
      overflow-y:auto;border-right:1px solid var(--border);padding-right:1rem;}
    .manual-toc .toc-head{font-family:var(--mono);font-size:.7rem;letter-spacing:.18em;
      text-transform:uppercase;color:var(--red-text);margin-bottom:.9rem;}
    .manual-toc nav{position:static;display:block;height:auto;padding:0;background:none;
      backdrop-filter:none;border:none;}
    .manual-toc a{display:block;color:var(--text-dim);font-size:.85rem;line-height:1.35;
      padding:.22rem 0;border:none;}
    .manual-toc a.t2{color:var(--text);font-weight:500;margin-top:.5rem;font-family:var(--mono);font-size:.82rem;}
    .manual-toc a.t3{padding-left:.85rem;font-size:.8rem;border-left:1px solid var(--border);}
    .manual-toc a:hover{color:var(--ink);text-decoration:none;}
    .manual-toc a.active{color:var(--red-text);}
    .manual-toc .t3group{margin:.15rem 0 .4rem;}

    /* content */
    .manual-body{min-width:0;font-size:1rem;}
    .manual-body h1{font-size:clamp(2rem,4vw,2.8rem);font-weight:700;letter-spacing:-0.02em;
      color:var(--ink);line-height:1.1;margin-bottom:1rem;}
    .manual-body h2{font-size:1.6rem;font-weight:700;letter-spacing:-0.02em;color:var(--ink);
      margin:2.8rem 0 1rem;padding-top:1.6rem;border-top:1px solid var(--border);scroll-margin-top:84px;}
    .manual-body h3{font-size:1.18rem;font-weight:700;color:var(--ink);margin:1.9rem 0 .7rem;
      font-family:var(--mono);scroll-margin-top:84px;}
    .manual-body h3 code{font-size:1em;background:none;padding:0;color:var(--ink);}
    .manual-body h4{font-size:1rem;font-weight:700;color:var(--text);margin:1.4rem 0 .5rem;scroll-margin-top:84px;}
    .manual-body p,.manual-body li{color:var(--text-dim);}
    .manual-body p{margin:.8rem 0;}
    .manual-body ul,.manual-body ol{margin:.7rem 0 .7rem 1.3rem;}
    .manual-body li{margin:.3rem 0;}
    .manual-body strong{color:var(--text);font-weight:700;}
    .manual-body a code{color:var(--red-text);}
    .manual-body code{font-family:var(--mono);font-size:.86em;background:var(--bg-card2);
      border:1px solid var(--border);border-radius:5px;padding:.08em .38em;color:#e6c9c6;}
    .manual-body pre{background:var(--bg-term);border:1px solid var(--border);border-radius:10px;
      padding:1rem 1.1rem;overflow-x:auto;margin:1rem 0;border-left:3px solid var(--red);}
    .manual-body pre code{background:none;border:none;padding:0;color:var(--text);font-size:.84rem;line-height:1.6;}
    .manual-body blockquote{border-left:3px solid var(--red);background:var(--red-glow);
      padding:.7rem 1rem;margin:1rem 0;border-radius:0 8px 8px 0;}
    .manual-body blockquote p{color:var(--text);margin:.3rem 0;}
    .manual-body hr{border:none;border-top:1px solid var(--border);margin:2.2rem 0;}
    .manual-body table{width:100%;border-collapse:collapse;margin:1.1rem 0;font-size:.9rem;display:block;overflow-x:auto;}
    .manual-body th,.manual-body td{border:1px solid var(--border);padding:.55rem .7rem;text-align:left;vertical-align:top;}
    .manual-body th{background:var(--bg-card2);color:var(--ink);font-weight:700;}
    .manual-body td{color:var(--text-dim);}

    /* footer */
    footer{border-top:1px solid var(--border);padding:2.5rem clamp(1rem,4vw,3rem);}
    .footer-inner{max-width:1180px;margin:0 auto;display:flex;flex-wrap:wrap;gap:1.2rem;align-items:center;justify-content:space-between;}
    .footer-brand{display:flex;align-items:center;gap:.5rem;font-family:var(--mono);color:var(--ink);}
    .footer-brand img{width:26px;height:26px;border-radius:6px;}
    .footer-brand .red{color:var(--red-text);}
    .footer-links{display:flex;flex-wrap:wrap;gap:1.1rem;}
    .footer-links a{color:var(--text-dim);font-size:.88rem;}
    .footer-legal{color:var(--text-faint);font-size:.82rem;font-family:var(--mono);}

    @media(max-width:900px){
      .manual-wrap{grid-template-columns:1fr;}
      .manual-toc{display:none;}
      .nav-links{display:none;flex-direction:column;position:absolute;top:64px;left:0;right:0;
        background:rgba(8,8,11,0.98);border-bottom:1px solid var(--border);padding:1.5rem 2rem;gap:1.2rem;}
      .nav-links.open{display:flex;}
      .nav-hamburger{display:flex;}
    }
  </style>'''

NAV = '''<nav>
  <a class="nav-logo" href="/">
    <img src="/_static/logo.png" alt="h5i">
    <span>h5i</span>
  </a>
  <ul class="nav-links" id="nav-links">
    <li><a href="/">Home</a></li>
    <li><a href="/features/">Features</a></li>
    <li><a href="/guides/">Guides</a></li>
    <li><a href="/workflows/">Workflows</a></li>
    <li><a href="/manual/" aria-current="page">Manual</a></li>
    <li><a href="/blog/">Blog</a></li>
    <li><a href="https://github.com/h5i-dev/h5i" class="nav-cta">GitHub →</a></li>
  </ul>
  <div class="nav-hamburger" id="hamburger" onclick="toggleNav()">
    <span></span><span></span><span></span>
  </div>
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

SCRIPT = '''<script>
  function toggleNav(){document.getElementById('nav-links').classList.toggle('open');}
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
