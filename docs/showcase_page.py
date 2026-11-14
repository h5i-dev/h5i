"""Render docs/SHOWCASE.md as the /showcase/ page.

SHOWCASE.md stays the one place a project is listed: contributors add a line
there, and this turns every `## Section` / `- [name](url): text` pair into a
card. A section this file has no copy for still renders, under its own heading,
except the ones in SKIPPED, which stay in the markdown only.
"""

import html
import re
from urllib.parse import urlparse

APPLY_URL = "https://docs.google.com/forms/d/e/1FAIpQLSf6S6feqo-5YG0b9Yj_Z5rmeG987H_AvpZAgCD883wMZWWlag/viewform?usp=dialog"
EDIT_URL = "https://github.com/h5i-dev/h5i/edit/main/docs/SHOWCASE.md"
CORE_OWNERS = {"h5i-dev", "koukyosyumei"}

# Copy for the sections SHOWCASE.md has today, matched on a heading prefix.
SECTIONS = (
    ("web applications", "apps", "A", "Apps",
     "Rust web applications whose core decisions live in h5i-app kernels, from version control to payments."),
    ("benchmarks", "bench", "B", "Bench",
     "Measurements anyone can rerun: verification coverage, runtime performance, and AI-driven pentesting."),
    ("third-party", "tools", "C", "Community",
     "Built by people outside the core team, on top of boxes, receipts, and browser audits."),
)

# Kept in SHOWCASE.md, left off the page.
SKIPPED = ("selected vulnerabilities",)

ITEM = re.compile(r"^- \[(?P<name>[^\]]+)\]\((?P<url>[^)]+)\):?\s*(?P<text>.*)$")


def inline(md):
    """Escape, then render the two inline forms SHOWCASE.md uses."""
    out = html.escape(md.strip(), quote=False)
    out = re.sub(r"`([^`]+)`", r"<code>\1</code>", out)
    return re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)",
                  lambda m: f'<a class="sc-inline" href="{html.escape(m[2])}">{m[1]}</a>', out)


def plain(md):
    return re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", md).replace("`", "").strip()


def parse(text):
    sections, skip = [], False
    for line in text.splitlines():
        line = line.rstrip()
        if line.startswith("## "):
            title = line[3:].strip()
            skip = title.lower().startswith(SKIPPED)
            if skip:
                continue
            key = title.lower()
            meta = next((m for m in SECTIONS if key.startswith(m[0])), None)
            if meta:
                _, slug, letter, label, blurb = meta
            else:
                slug = re.sub(r"[^a-z0-9]+", "-", plain(key)).strip("-")
                letter, label, blurb = chr(ord("A") + len(sections)), plain(title), ""
            sections.append({"title": title, "slug": slug, "letter": letter,
                             "label": label, "blurb": blurb, "items": []})
        elif not skip and (m := ITEM.match(line)) and sections:
            sections[-1]["items"].append(m.groupdict())
    return [s for s in sections if s["items"]]


def repo_path(url):
    parts = urlparse(url)
    bits = [b for b in parts.path.split("/") if b]
    return parts.netloc.removeprefix("www."), bits


# Star counts come from shields.io, styled to sit on the dark card.
BADGE = ("https://img.shields.io/github/stars/{repo}?style=flat-square&logo=github"
         "&label=&labelColor=151513&color=1d1d1b&logoColor=a5a198")


def stars(host, bits):
    if host != "github.com" or len(bits) < 2:
        return ""
    repo = "/".join(bits[:2])
    return (f'<img class="sc-stars" src="{html.escape(BADGE.format(repo=repo))}" '
            f'alt="GitHub stars for {html.escape(repo)}" height="20" loading="lazy" decoding="async">')


def card(item, code):
    host, bits = repo_path(item["url"])
    owner = bits[0] if bits else host
    where = "/".join(bits[:2]) if host == "github.com" else host
    core = owner.lower() in CORE_OWNERS
    tag = "core" if core else f"@{html.escape(owner)}"
    return f"""<article class="sc-card{'' if core else ' community'}" data-reveal>
<div class="sc-card-top"><span class="sc-code">{code}</span><span class="sc-tag">{tag}</span></div>
<h3><a class="sc-main" href="{html.escape(item['url'])}">{html.escape(item['name'])}</a></h3>
<p>{inline(item['text'])}</p>
<div class="sc-card-foot"><span class="sc-repo">{html.escape(where)}</span>{stars(host, bits)}<span class="sc-go" aria-hidden="true">&#8599;</span></div>
</article>"""


# The hero window: the loop a builder runs, in three tabs. Same kernel and
# theorem as the landing page's stage, so the two pages tell one story.
RUST = """use h5i_app::prelude::*;

fn read_order(user: u64, s: &Snapshot, id: u64) -> Outcome {
    match find_order(&s.orders, id) {
        None => Err(Error::NotFound),
        Some(o) => {
            if o.owner == user {
                Ok((none(), Reply::Order(o.clone())))
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}"""

LEAN = """-- what a reply may show to the user who asked
def visible (s : St) (u : Nat) : Reply → Prop
  | .Order o => o.owner.val = u
  | _ => True

theorem owner_only (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    visible (Snapshot.toSt s) a.user.val r := by
  rcases reply_of a s c ws r h with ⟨o, rfl, hown⟩
  simp [visible, hown]"""

SHELL = [
    ("$", "h5i app new shop"),
    ("$", "h5i app extract"),
    ("d", "  charon   kernel → shop_kernel.llbc"),
    ("d", "  aeneas   shop_kernel.llbc → proofs/generated/"),
    ("$", "h5i app prove"),
    ("d", "  lake     build Theorems"),
    ("g", "  owner_only            proven"),
    ("g", "  inv_preserved         proven"),
    ("$", "h5i app mutate --auto"),
    ("r", "  mutant   o.owner == user  →  true"),
    ("g", "  owner_only fails: 14 of 14 mutants caught"),
]

KEYWORDS = {
    "rs": "use fn match if else let return true false Some None Ok Err",
    "lean": "def theorem by fun do let match with if then else rcases simp rfl",
}


def tint(code, lang):
    """Highlight a source line on the server, so the page needs no highlighter."""
    words = set(KEYWORDS[lang].split())
    out = []
    for tok in re.findall(r"//.*$|--.*$|[A-Za-z_][\w.]*|\d+|\s+|.", code):
        t = html.escape(tok, quote=False)
        if tok.startswith(("//", "--")):
            out.append(f'<span class="t-c">{t}</span>')
        elif tok in words:
            out.append(f'<span class="t-k">{t}</span>')
        elif tok[:1].isupper():
            out.append(f'<span class="t-t">{t}</span>')
        elif tok[:1].isdigit():
            out.append(f'<span class="t-n">{t}</span>')
        else:
            out.append(t)
    return "".join(out)


def window():
    def source(text, lang):
        return "".join(f'<span class="t-line"><i>{n}</i>{tint(line, lang)}</span>'
                       for n, line in enumerate(text.splitlines(), 1))
    shell = ""
    for kind, text in SHELL:
        if kind == "$":
            shell += f'<span class="t-line t-cmd"><span class="t-p">$</span> <span class="t-in">{html.escape(text)}</span></span>'
        else:
            shell += f'<span class="t-line t-{kind}">{html.escape(text)}</span>'
    shell += '<span class="t-line"><span class="t-p">$</span> <span class="t-caret"></span></span>'
    panes = (("kernel/src/lib.rs", "rust", source(RUST, "rs")),
             ("proofs/Theorems.lean", "lean 4", source(LEAN, "lean")),
             ("~/shop", "zsh", shell))
    tabs = "".join(f'<button type="button" class="sc-tab" role="tab" aria-selected="{str(i == 2).lower()}" data-pane="{i}">{html.escape(name)}</button>'
                   for i, (name, _, _) in enumerate(panes))
    bodies = "".join(f'<pre class="sc-term-body{" src" if i < 2 else ""}" role="tabpanel" data-pane="{i}" data-lang="{lang}"{"" if i == 2 else " hidden"}>{body}</pre>'
                     for i, (_, lang, body) in enumerate(panes))
    return f"""<div class="sc-term" aria-label="Write a Rust kernel, state a Lean 4 theorem, and prove it with h5i app">
<div class="sc-term-bar" role="tablist">{tabs}<span class="sc-term-lang">zsh</span></div>
{bodies}</div>"""


def render(text, head, nav, footer, modified):
    sections = parse(text)
    total = sum(len(s["items"]) for s in sections)
    url = "https://h5i.dev/showcase/"
    title = "h5i Showcase: apps, benchmarks, and community tools"
    description = (f"{total} projects built with h5i: Rust web apps on h5i-app, benchmarks, "
                   "and community tools.")
    entries = [(s, i) for s in sections for i in s["items"]]
    schema = {"@context": "https://schema.org", "@graph": [
        {"@type": "CollectionPage", "@id": f"{url}#page", "url": url, "name": title,
         "description": description, "inLanguage": "en", "dateModified": modified,
         "isPartOf": {"@id": "https://h5i.dev/#website"}, "about": {"@id": "https://h5i.dev/#app"}},
        {"@type": "BreadcrumbList", "@id": f"{url}#breadcrumb", "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Home", "item": "https://h5i.dev/"},
            {"@type": "ListItem", "position": 2, "name": "Showcase", "item": url},
        ]},
        {"@type": "ItemList", "@id": f"{url}#list", "name": title, "itemListElement": [
            {"@type": "ListItem", "position": n + 1, "url": item["url"], "name": item["name"]}
            for n, (_, item) in enumerate(entries)]},
    ]}

    stats = "".join(f"""<a class="sc-stat" href="#{s['slug']}"><span class="sc-stat-n" data-count="{len(s['items'])}">{len(s['items']):02d}</span>
<span class="sc-stat-l">{html.escape(s['label'])}</span></a>""" for s in sections)
    filters = '<button type="button" class="sc-chip" aria-pressed="true" data-filter="all">All <span>' + f"{total:02d}</span></button>"
    filters += "".join(f'<button type="button" class="sc-chip" aria-pressed="false" data-filter="{s["slug"]}">{html.escape(s["label"])} <span>{len(s["items"]):02d}</span></button>'
                       for s in sections)

    bands = ""
    for s in sections:
        body = "".join(card(item, f"{s['letter']}.{n:02d}") for n, item in enumerate(s["items"], 1))
        bands += f"""<section class="sc-band" id="{s['slug']}" data-band="{s['slug']}">
<header class="sc-band-head" data-reveal><span class="sc-band-letter" aria-hidden="true">{s['letter']}</span>
<div><div class="sc-kicker">{s['letter']} / {html.escape(s['label'])} &middot; {len(s['items']):02d}</div>
<h2>{inline(s['title'])}</h2>{f"<p>{html.escape(s['blurb'])}</p>" if s['blurb'] else ""}</div></header>
<div class="sc-grid">{body}</div></section>"""

    page_nav = nav.replace('<li><a href="/showcase/">', '<li><a href="/showcase/" aria-current="page">')
    top = head(title, description, url, schema, kind="website").replace(
        "</head>", '<link rel="stylesheet" href="/_static/showcase.css"></head>')
    return f"""{top}
<body class="sc-page">{page_nav}<main class="sc">
<section class="sc-hero">
<div class="sc-hero-copy">
<div class="post-eyebrow">Showcase / {total} entries</div>
<h1>Built with h5i<span class="sc-dot">.</span></h1>
<p class="sc-deck">Verified web apps, reproducible benchmarks, and tools built by the community. Everything here started as one line in <a href="https://github.com/h5i-dev/h5i/blob/main/docs/SHOWCASE.md"><code>SHOWCASE.md</code></a>. The next line could be yours.</p>
<div class="hero-actions"><a class="btn btn-primary" href="{APPLY_URL}">Join the Early Builder Program &rarr;</a>
<a class="btn btn-ghost" href="{EDIT_URL}">Add your project</a></div>
</div>
{window()}
</section>
<section class="sc-stats">{stats}</section>
<div class="sc-filter" role="toolbar" aria-label="Filter the showcase">{filters}</div>
{bands}
<section class="sc-cta" id="early-builders">
<div class="sc-cta-copy" data-reveal>
<div class="sc-kicker">Early Builder Program</div>
<h2>Build on h5i.<br>Ship it here.</h2>
<p>We are looking for people building real things with h5i-app, boxes, and the red-teaming tools. Tell us what you are making and how we can help.</p>
<div class="hero-actions"><a class="btn btn-primary sc-cta-btn" href="{APPLY_URL}">Apply now &rarr;</a></div>
</div>
<ol class="sc-steps" data-reveal>
<li><span class="sc-step-n">01</span><div><h3>Build</h3><p>Start from <code>h5i app new</code>, wrap an agent in a box, or point the red-team tools at an app you are authorized to test.</p></div></li>
<li><span class="sc-step-n">02</span><div><h3>Apply</h3><p>Fill in the <a class="sc-inline" href="{APPLY_URL}">Early Builder Program form</a>. It takes a few minutes.</p></div></li>
<li><span class="sc-step-n">03</span><div><h3>Get listed</h3><p>Open a pull request adding one line to <a class="sc-inline" href="{EDIT_URL}"><code>docs/SHOWCASE.md</code></a>.</p></div></li>
</ol>
</section>
</main>{footer}<script src="/_static/showcase.js" defer></script></body></html>"""
