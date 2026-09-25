import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { Fleet } from "./App";
import {
  api,
  type GlossaryTerm,
  type ProjectChecklist,
  type ProjectDetail,
  type ProjectFinding,
  type ProjectSummary,
  type ReportView,
  type SessionRow,
} from "./api";
import { AttentionTag, Chip, Cmd, Count, Empty, Note, Split, ago, plural } from "./ui";
import { shownState } from "./seen";

// A project is the durable engagement: notes, findings, the evidence they rest
// on, the checklists they cover and the reports issued from them. It outlives
// the sessions it came from, which is the whole point — a session is
// disposable, its conclusions are not.

type Tab = "overview" | "findings" | "coverage" | "notes" | "report" | "sessions";

const TABS: { key: Tab; label: string }[] = [
  { key: "overview", label: "Overview" },
  { key: "findings", label: "Findings" },
  { key: "coverage", label: "Coverage" },
  { key: "notes", label: "Notes" },
  { key: "report", label: "Report" },
  { key: "sessions", label: "Sessions" },
];

const SEV_TONE: Record<string, "bad" | "warn" | "info" | "dim"> = {
  critical: "bad",
  high: "bad",
  medium: "warn",
  low: "info",
  info: "dim",
  unrated: "dim",
};

function sevTone(sev: string) {
  return SEV_TONE[sev || "unrated"] ?? "dim";
}

/** Sessions grouped by the project label they carry. */
function sessionsByProject(sessions: SessionRow[]): Map<string, SessionRow[]> {
  const by = new Map<string, SessionRow[]>();
  for (const s of sessions) {
    if (!s.project) continue;
    const list = by.get(s.project);
    if (list) list.push(s);
    else by.set(s.project, [s]);
  }
  return by;
}

export function ProjectsPage({
  fleet,
  route,
  go,
}: {
  fleet: Fleet;
  route: string[];
  go: (parts: string[], replace?: boolean) => void;
}) {
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const sessions = fleet.sessions?.sessions ?? [];
  const sessionGroups = useMemo(() => sessionsByProject(sessions), [sessions]);

  useEffect(() => {
    api
      .projects()
      .then((p) => {
        setProjects(p);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [fleet.tick]);

  // Durable projects, plus any project label a session carries that has no
  // store yet — so a run under `--project acme` is visible before `init`.
  const names = useMemo(() => {
    const set = new Set<string>((projects ?? []).map((p) => p.name));
    for (const name of sessionGroups.keys()) set.add(name);
    return [...set].sort();
  }, [projects, sessionGroups]);

  const selected = route[0] ?? null;

  if (projects && names.length === 0) {
    return (
      <Empty title="No projects yet">
        <p>
          A project is the durable side of an engagement. Findings, the evidence they rest on, notes and reports
          live in it and survive a session ending or being removed.
        </p>
        <Cmd text="h5i project init acme --title 'ACME web' --target https://acme.test" />
        <p>Then open a browser session under it, and promote what you find:</p>
        <Cmd text="h5i browser open https://acme.test --project acme --capture" />
        <Cmd text="h5i project finding promote --all -p acme --session <name>" />
      </Empty>
    );
  }

  return (
    <Split
      id="projects"
      first={
        <div className="column">
          <div className="column-body">
            {error ? <Note tone="bad">{error}</Note> : null}
            {names.map((name) => {
              const p = (projects ?? []).find((x) => x.name === name) ?? null;
              const sess = sessionGroups.get(name) ?? [];
              const live = sess.filter((s) => s.state === "live").length;
              return (
                <button
                  key={name}
                  type="button"
                  className={`srow${name === selected ? " is-on" : ""}`}
                  onClick={() => go(["projects", name])}
                >
                  <div className="srow-top">
                    <span className="srow-name">{p?.title || name}</span>
                    <span className="srow-age">{ago(p?.updated ?? null)}</span>
                  </div>
                  <div className="srow-target">
                    {p ? name : `${name} — label only, not initialised`}
                  </div>
                  <div className="srow-bottom">
                    <div className="srow-counts">
                      {p && p.findings > 0 ? (
                        <Count n={p.findings} label={plural(p.findings, "finding")} tone={p.open_findings > 0 ? "warn" : "good"} />
                      ) : null}
                      {sess.length > 0 ? <Count n={sess.length} label={plural(sess.length, "session")} /> : null}
                      {live > 0 ? <Count n={live} label="live" tone="good" /> : null}
                      {p && p.reports > 0 ? <Count n={p.reports} label={plural(p.reports, "report")} tone="info" /> : null}
                    </div>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      }
      second={
        selected ? (
          <ProjectView
            name={selected}
            hasStore={(projects ?? []).some((p) => p.name === selected)}
            sessions={sessionGroups.get(selected) ?? []}
            fleet={fleet}
            route={route.slice(1)}
            go={go}
          />
        ) : null
      }
    />
  );
}

function ProjectView({
  name,
  hasStore,
  sessions,
  fleet,
  route,
  go,
}: {
  name: string;
  hasStore: boolean;
  sessions: SessionRow[];
  fleet: Fleet;
  route: string[];
  go: (parts: string[], replace?: boolean) => void;
}) {
  const [detail, setDetail] = useState<ProjectDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // Blank the pane only when the project itself changes. A poll (`fleet.tick`)
  // refetches quietly below and swaps the data in place, so the tab — the
  // report especially — is never torn down and refetched under the reader.
  useEffect(() => {
    setDetail(null);
  }, [name]);

  useEffect(() => {
    if (!hasStore) return;
    let live = true;
    api
      .project(name)
      .then((d) => {
        if (!live) return;
        setDetail(d);
        setErr(null);
      })
      .catch((e) => live && setErr(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [name, hasStore, fleet.tick]);

  const tab = (TABS.find((t) => t.key === route[0])?.key ?? "overview") as Tab;
  const setTab = (t: Tab) => go(["projects", name, t]);

  if (!hasStore) {
    return (
      <div className="work">
        <div className="work-head">
          <div className="work-title">
            <h2>{name}</h2>
            <Chip tone="dim">label only</Chip>
          </div>
        </div>
        <div className="work-body">
          <div className="scroll pad">
            <Note>
              Sessions carry <code>--project {name}</code>, but no project store exists yet. Initialise it to keep
              findings, evidence and a report that outlive those sessions.
            </Note>
            <Cmd text={`h5i project init ${name}`} />
            <h3>Sessions under this label</h3>
            <SessionList sessions={sessions} go={go} />
          </div>
        </div>
      </div>
    );
  }

  if (err) {
    return (
      <div className="work">
        <div className="work-body">
          <Empty title="Could not read this project">
            <p>{err}</p>
          </Empty>
        </div>
      </div>
    );
  }
  if (!detail) {
    return (
      <div className="work">
        <div className="work-body">
          <div className="scroll pad">
            <Note>reading…</Note>
          </div>
        </div>
      </div>
    );
  }

  const s = detail.summary;
  const counts: Record<Tab, number | null> = {
    overview: null,
    findings: detail.findings.length,
    coverage: detail.checklists.length,
    notes: detail.notes.length,
    report: detail.reports.length,
    sessions: sessions.length,
  };

  return (
    <div className="work">
      <div className="work-head">
        <div className="work-title">
          <h2>{detail.meta.title || name}</h2>
          {s.open_findings > 0 ? <Chip tone="warn">{s.open_findings} open</Chip> : null}
        </div>
        <div className="work-sub">
          {detail.meta.targets.length > 0 ? <span>{detail.meta.targets.join(", ")}</span> : null}
          <span>{s.findings} {plural(s.findings, "finding")}</span>
          <span>{s.evidence} evidence</span>
          <span>{s.reports} {plural(s.reports, "report")}</span>
          <span>started {ago(s.created)}</span>
        </div>
        <div className="tabs" role="tablist">
          {TABS.map((t) => (
            <button
              key={t.key}
              type="button"
              role="tab"
              aria-selected={tab === t.key}
              className={`tab${tab === t.key ? " is-on" : ""}`}
              onClick={() => setTab(t.key)}
            >
              {t.label}
              {counts[t.key] !== null ? (
                <span className={`tab-n${t.key === "findings" && detail.summary.open_findings > 0 ? " is-loud" : ""}`}>
                  {counts[t.key]}
                </span>
              ) : null}
            </button>
          ))}
        </div>
      </div>
      <div className="work-body">
        {tab === "overview" ? (
          <OverviewTab detail={detail} sessions={sessions} setTab={setTab} />
        ) : tab === "findings" ? (
          <FindingsTab detail={detail} />
        ) : tab === "coverage" ? (
          <CoverageTab detail={detail} />
        ) : tab === "notes" ? (
          <NotesTab detail={detail} />
        ) : tab === "report" ? (
          <ReportTab name={name} detail={detail} />
        ) : (
          <div className="scroll pad">
            <SessionList sessions={sessions} go={go} />
          </div>
        )}
      </div>
    </div>
  );
}

function OverviewTab({
  detail,
  sessions,
  setTab,
}: {
  detail: ProjectDetail;
  sessions: SessionRow[];
  setTab: (t: Tab) => void;
}) {
  const open = detail.findings.filter((f) => ["", "open", "in-progress", "fix-claimed"].includes(f.status));
  const top = open.slice(0, 5);
  const cov = detail.checklists;
  const latest = detail.reports.length ? detail.reports[detail.reports.length - 1] : null;
  return (
    <div className="scroll pad ov">
      <section className="ov-section">
        <h3>What needs attention</h3>
        {top.length === 0 ? (
          <Note tone="good">No open findings. {detail.findings.length > 0 ? "Every finding is resolved or accepted." : "Nothing recorded yet."}</Note>
        ) : (
          <div className="plist">
            {top.map((f) => (
              <button key={f.id} type="button" className="srow" onClick={() => setTab("findings")}>
                <div className="srow-top">
                  <span className="srow-name">
                    <code>{f.id}</code> {f.title}
                  </span>
                  <Chip tone={sevTone(f.severity)}>{f.severity || "unrated"}</Chip>
                </div>
                {f.summary ? <div className="srow-target">{f.summary}</div> : null}
              </button>
            ))}
          </div>
        )}
      </section>

      <section className="ov-section">
        <h3>Coverage</h3>
        {cov.length === 0 ? (
          <Note>
            No checklist imported, so what was looked at is the findings, not a plan. Import one to track it:
            <Cmd text={`h5i project checklist import -p ${detail.meta.name} checks.md --required`} />
          </Note>
        ) : (
          cov.map((c) => (
            <p key={c.checklist.slug} className="count">
              <b>{c.checklist.title}</b>: {coverageSentence(c)}
            </p>
          ))
        )}
      </section>

      <section className="ov-section">
        <h3>Report</h3>
        {latest ? (
          <p className="count">
            Latest issued: <b>v{latest.version}</b>, {ago(latest.issued)}, {latest.findings} {plural(latest.findings, "finding")}.{" "}
            <button type="button" className="linklike" onClick={() => setTab("report")}>
              open
            </button>
          </p>
        ) : detail.has_draft ? (
          <p className="count">
            A draft exists but nothing is issued yet.{" "}
            <button type="button" className="linklike" onClick={() => setTab("report")}>
              open the draft
            </button>
          </p>
        ) : (
          <Note>
            No report yet.
            <Cmd text={`h5i project report new -p ${detail.meta.name}`} />
          </Note>
        )}
      </section>

      {sessions.length > 0 ? (
        <section className="ov-section">
          <p className="count">
            {sessions.length} {plural(sessions.length, "session")} under this project.{" "}
            <button type="button" className="linklike" onClick={() => setTab("sessions")}>
              list
            </button>
          </p>
        </section>
      ) : null}
    </div>
  );
}

function coverageSentence(c: ProjectChecklist): string {
  const cov = c.coverage;
  const parts = [`${cov.closed} of ${cov.total} items have an outcome`];
  const count = (s: string) => cov.by_status.find(([k]) => k === s)?.[1] ?? 0;
  for (const s of ["open", "in-progress", "blocked", "not-applicable"]) {
    const n = count(s);
    if (n > 0) parts.push(`${n} ${s}`);
  }
  if (cov.dropped > 0) parts.push(`${cov.dropped} dropped in a revision`);
  return parts.join("; ");
}

function FindingsTab({ detail }: { detail: ProjectDetail }) {
  const name = detail.meta.name;
  if (detail.findings.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No findings recorded">
          <p>Write one, or promote what a session already found into the project with its evidence.</p>
          <Cmd text={`h5i project finding create -p ${name} --title '…' --severity high`} />
          <Cmd text={`h5i project finding promote --all -p ${name} --session <name>`} />
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      {detail.findings.map((f) => (
        <FindingCard key={f.id} f={f} name={name} evidenceCaptions={captionsFor(detail)} />
      ))}
    </div>
  );
}

function captionsFor(detail: ProjectDetail): Record<string, string> {
  const out: Record<string, string> = {};
  for (const e of detail.evidence) out[e.id] = e.caption || e.url || e.id;
  return out;
}

function FindingCard({ f, name, evidenceCaptions }: { f: ProjectFinding; name: string; evidenceCaptions: Record<string, string> }) {
  const [open, setOpen] = useState(false);
  return (
    <article className="finding">
      <div className="finding-head">
        <span className="finding-id">{f.id}</span>
        <h3>{f.title || "(untitled)"}</h3>
        <Chip tone={sevTone(f.severity)}>{f.severity || "unrated"}</Chip>
        <Chip tone={f.status === "fixed-verified" ? "good" : f.status === "risk-accepted" ? "dim" : "plain"}>{f.status || "open"}</Chip>
        {f.state ? <Chip tone="dim" title="the assessor's confidence, separate from severity">{f.state}</Chip> : null}
        <span className="finding-when" title={`written ${f.created}, last changed ${f.updated}`}>{ago(f.updated)}</span>
      </div>
      {f.summary ? <p className="finding-summary">{f.summary}</p> : null}
      <button type="button" className="linklike" onClick={() => setOpen(!open)}>
        {open ? "less" : "impact, remediation, evidence"}
      </button>
      {open ? (
        <div className="finding-detail">
          {field("Impact", f.impact)}
          {field("Affected", f.affected)}
          {field("Why this severity", f.severity_reason)}
          {field("Remediation", f.remediation)}
          {f.evidence.length > 0 ? (
            <div className="finding-part">
              <h4>Evidence</h4>
              <div className="chips">
                {f.evidence.map((e) => (
                  <Chip key={e} tone="info" mono title={evidenceCaptions[e] ?? e}>
                    {e}
                  </Chip>
                ))}
              </div>
            </div>
          ) : null}
          {f.sources.length > 0 ? (
            <p className="count">
              from {f.sources.map((s) => `${s.finding} of ${s.session_name ?? s.session}`).join(", ")}
            </p>
          ) : null}
          {f.notes.length > 0 ? (
            <ul className="finding-notes">
              {f.notes.map((n, i) => (
                <li key={i}>
                  <time title={n.at}>{ago(n.at)}</time> {n.text}
                </li>
              ))}
            </ul>
          ) : null}
          {f.body.trim() ? <pre className="finding-body">{f.body}</pre> : null}
          <Cmd text={`h5i project finding show ${f.id} -p ${name}`} hint="the finding in full, on the CLI" />
        </div>
      ) : null}
    </article>
  );
}

function field(label: string, value: string) {
  if (!value.trim()) return null;
  return (
    <div className="finding-part">
      <h4>{label}</h4>
      <p>{value}</p>
    </div>
  );
}

function CoverageTab({ detail }: { detail: ProjectDetail }) {
  if (detail.checklists.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No checklist imported">
          <p>
            A checklist is any Markdown list: an internal standard, a client's list, last round's notes. Its items
            become tracked, and a required list has to end each item with an outcome.
          </p>
          <Cmd text={`h5i project checklist import -p ${detail.meta.name} checks.md --required`} />
          <p>
            Coverage is counts, never a score: "18 of 24 have an outcome" says what was looked at, not that the rest
            is safe.
          </p>
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      {detail.checklists.map((c) => (
        <section key={c.checklist.slug} className="checklist-block">
          <div className="section-head">
            <h3>
              {c.checklist.title} <Chip tone={c.checklist.mode === "required" ? "warn" : "dim"}>{c.checklist.mode}</Chip>
            </h3>
          </div>
          <p className="count">{coverageSentence(c)}</p>
          <table className="ptable">
            <tbody>
              {c.outcomes.map((o) => (
                <tr key={o.item.id}>
                  <td className="mono">{o.item.id}</td>
                  <td>{o.item.text}</td>
                  <td>
                    <Chip
                      tone={
                        o.status === "recorded"
                          ? "good"
                          : o.status === "blocked"
                            ? "warn"
                            : o.status === "not-applicable"
                              ? "dim"
                              : "plain"
                      }
                    >
                      {o.status}
                    </Chip>
                  </td>
                  <td className="count">{o.notes.length ? o.notes[o.notes.length - 1][1] : ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      ))}
    </div>
  );
}

function NotesTab({ detail }: { detail: ProjectDetail }) {
  if (detail.notes.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No notes">
          <p>Notes are free text: a hypothesis, what the owner told you, a missing account. A note is never counted as a finding.</p>
          <Cmd text={`h5i project note add -p ${detail.meta.name} 'this feature is admin-only, per the owner'`} />
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      <ul className="note-list">
        {detail.notes.map((n) => (
          <li key={n.id}>
            <div className="note-meta">
              <code>{n.id}</code>
              {n.tags.map((t) => (
                <Chip key={t} tone="dim">
                  {t}
                </Chip>
              ))}
              <time title={n.updated}>{ago(n.updated)}</time>
            </div>
            <p>{n.text}</p>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ReportTab({ name, detail }: { name: string; detail: ProjectDetail }) {
  const versions = detail.reports.map((r) => r.version);
  const [version, setVersion] = useState<number | null>(versions.length ? versions[versions.length - 1] : null);
  const [view, setView] = useState<ReportView | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [glossary, setGlossary] = useState<Record<string, GlossaryTerm>>({});
  const [panel, setPanel] = useState<GlossaryTerm | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api
      .glossary()
      .then((terms) => setGlossary(Object.fromEntries(terms.map((t) => [t.id, t]))))
      .catch(() => setGlossary({}));
  }, []);

  useEffect(() => {
    setView(null);
    setErr(null);
    api
      .report(name, version ?? undefined)
      .then(setView)
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)));
  }, [name, version]);

  // A term in the report is a button: click it, and its plain-language
  // definition opens beside the page. Keyboard reaches it too.
  const onBodyClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      const el = (e.target as HTMLElement).closest(".term") as HTMLElement | null;
      if (!el) return;
      const id = el.dataset.term;
      if (id && glossary[id]) setPanel(glossary[id]);
    },
    [glossary],
  );

  if (!detail.has_draft && versions.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No report yet">
          <p>Start the report from the built-in template, write the prose, then let h5i check and freeze it.</p>
          <Cmd text={`h5i project report new -p ${name}`} />
          <Cmd text={`h5i project report check -p ${name}`} hint="dangling ids and missing outcomes are errors" />
          <Cmd text={`h5i project report issue -p ${name}`} hint="freeze it as a versioned snapshot" />
        </Empty>
      </div>
    );
  }

  return (
    <div className="report-tab">
      <div className="report-bar">
        <div className="report-versions">
          <Chip tone="dim" on={version === null} onClick={() => setVersion(null)} title="the live working draft">
            draft
          </Chip>
          {versions.map((v) => (
            <Chip key={v} tone="info" on={version === v} onClick={() => setVersion(v)}>
              v{v}
            </Chip>
          ))}
        </div>
        <div className="report-actions">
          <Cmd text={`h5i project report export -p ${name}${version != null ? ` --version ${version}` : ""} --format pdf`} hint="write a PDF (needs a local Chromium)" />
        </div>
      </div>
      <div className="report-frame">
        {view && view.toc.length > 0 ? (
          <nav className="report-toc">
            {view.toc.map((t) => (
              <a key={t.id} href={`#${t.id}`} className={`toc-l${t.level}`}>
                {t.title}
              </a>
            ))}
          </nav>
        ) : null}
        <div className="report-scroll">
          {err ? (
            <Note tone="bad">{err}</Note>
          ) : !view ? (
            <Note>rendering…</Note>
          ) : (
            <div
              ref={bodyRef}
              className="report-html"
              onClick={onBodyClick}
              // The HTML is h5i's own renderer output: directives resolved,
              // raw HTML escaped, only http(s)/mailto links kept.
              dangerouslySetInnerHTML={{ __html: view.html }}
            />
          )}
        </div>
        {panel ? <TermPanel term={panel} onClose={() => setPanel(null)} /> : null}
      </div>
    </div>
  );
}

function TermPanel({ term, onClose }: { term: GlossaryTerm; onClose: () => void }) {
  return (
    <aside className="term-panel">
      <div className="term-panel-head">
        <h4>{term.term}</h4>
        <button type="button" className="linklike" onClick={onClose} aria-label="close">
          ✕
        </button>
      </div>
      <p>{term.explain}</p>
      {term.link ? (
        <p>
          <a href={term.link} target="_blank" rel="noreferrer">
            {term.link_label ?? "learn more"} ↗
          </a>
        </p>
      ) : null}
    </aside>
  );
}

function SessionList({ sessions, go }: { sessions: SessionRow[]; go: (parts: string[]) => void }) {
  if (sessions.length === 0) {
    return <Note>No session carries this project label.</Note>;
  }
  return (
    <div className="plist">
      {sessions.map((s) => (
        <button key={s.id} type="button" className="srow" onClick={() => go(["sessions", s.id])}>
          <div className="srow-top">
            <span className="srow-name">{s.name ?? s.id}</span>
            <span className="srow-age">{ago(s.last_request_at ?? s.started_at)}</span>
          </div>
          <div className="srow-target">{s.url}</div>
          <div className="srow-bottom">
            <div className="srow-counts">
              <Count n={s.requests} label="fetches" />
              {s.findings > 0 ? <Count n={s.findings} label={plural(s.findings, "finding")} tone="warn" /> : null}
            </div>
            <AttentionTag state={shownState(s, {})} why={s.attention.why} />
          </div>
        </button>
      ))}
    </div>
  );
}
