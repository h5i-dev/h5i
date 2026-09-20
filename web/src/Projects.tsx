import { useMemo } from "react";

import type { Fleet } from "./App";
import type { SessionRow } from "./api";
import { ATTENTION_ORDER, AttentionTag, Chip, Cmd, Count, Empty, Note, Split, ago, plural } from "./ui";
import { shownState } from "./seen";

// An engagement, not a session. A session name is reused once its session has
// ended, so `project` is the only durable way to ask "everything that touched
// this target". Every number here is a fold over the sessions the poll read.

export interface Project {
  name: string;
  sessions: SessionRow[];
  live: number;
  requests: number;
  denied: number;
  findings: number;
  origins: string[];
  /** One digest, or the count when sessions disagree about the scope. */
  scopes: string[];
  loudest: SessionRow | null;
  lastAt: string | null;
}

export function fold(sessions: SessionRow[]): Project[] {
  const by = new Map<string, SessionRow[]>();
  for (const s of sessions) {
    if (!s.project) continue;
    const list = by.get(s.project);
    if (list) list.push(s);
    else by.set(s.project, [s]);
  }
  const out: Project[] = [];
  for (const [name, rows] of by) {
    const origins = [...new Set(rows.flatMap((r) => r.origins))].sort();
    const scopes = [...new Set(rows.map((r) => r.scope_digest).filter(Boolean))];
    const loudest = [...rows].sort(
      (a, b) => ATTENTION_ORDER.indexOf(a.attention.state) - ATTENTION_ORDER.indexOf(b.attention.state),
    )[0] ?? null;
    const stamps = rows.map((r) => r.last_request_at ?? r.started_at).filter(Boolean) as string[];
    out.push({
      name,
      sessions: rows,
      live: rows.filter((r) => r.state === "live").length,
      requests: rows.reduce((n, r) => n + r.requests, 0),
      denied: rows.reduce((n, r) => n + r.denied, 0),
      findings: rows.reduce((n, r) => n + r.findings, 0),
      origins,
      scopes,
      loudest,
      lastAt: stamps.length > 0 ? stamps.sort()[stamps.length - 1] : null,
    });
  }
  // Live work first, then whatever moved most recently.
  return out.sort((a, b) => b.live - a.live || (b.lastAt ?? "").localeCompare(a.lastAt ?? ""));
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
  const sessions = fleet.sessions?.sessions ?? null;
  const projects = useMemo(() => fold(sessions ?? []), [sessions]);
  const name = route[0] ?? null;
  const project = projects.find((p) => p.name === name) ?? null;

  if (sessions && projects.length === 0) {
    return (
      <Empty title="No session carries a project">
        <p>
          A project is the engagement a session belongs to. It is written on the record at open, and it is what
          survives a session ending and its name being reused.
        </p>
        <Cmd text="h5i browser open https://target.example --project acme" />
        <p>
          When <code>~/.config/h5i/projects/acme.toml</code> exists it is also the scope in force, and its digest
          goes on every session opened under it.
        </p>
      </Empty>
    );
  }

  return (
    <Split
      id="projects"
      first={
        <div className="column">
          <div className="column-body">
            {projects.map((p) => (
            <button
              key={p.name}
              type="button"
              className={`srow${p.name === name ? " is-on" : ""}`}
              onClick={() => go(["projects", p.name])}
            >
              <div className="srow-top">
                <span className="srow-name">{p.name}</span>
                <span className="srow-age">{ago(p.lastAt)}</span>
              </div>
              <div className="srow-target">
                {p.origins[0] ?? "nothing reached yet"}
                {p.origins.length > 1 ? ` +${p.origins.length - 1}` : ""}
              </div>
              <div className="srow-bottom">
                <div className="srow-counts">
                  <Count n={p.sessions.length} label={plural(p.sessions.length, "session")} />
                  {p.live > 0 ? <Count n={p.live} label="live" tone="good" /> : null}
                  {p.findings > 0 ? <Count n={p.findings} label={plural(p.findings, "finding")} tone="warn" /> : null}
                  {p.denied > 0 ? <Count n={p.denied} label="refused" tone="bad" /> : null}
                </div>
                {p.loudest ? (
                  <AttentionTag state={shownState(p.loudest, {})} why={p.loudest.attention.why} />
                ) : null}
              </div>
              </button>
            ))}
          </div>
        </div>
      }
      second={project ? <ProjectView project={project} go={go} /> : null}
    />
  );
}

function ProjectView({
  project,
  go,
}: {
  project: Project;
  go: (parts: string[], replace?: boolean) => void;
}) {
  return (
    <div className="work">
      <div className="work-head">
        <div className="work-title">
          <h2>{project.name}</h2>
          {project.live > 0 ? <Chip tone="good">{project.live} live</Chip> : null}
          {project.denied > 0 ? (
            <Chip tone="bad" title="fetches policy refused before the wire">
              {project.denied} refused
            </Chip>
          ) : null}
          {project.scopes.length > 1 ? (
            <Chip tone="warn" title="results gathered under different scopes are not comparable">
              {project.scopes.length} scopes
            </Chip>
          ) : null}
        </div>
        <div className="work-sub">
          <span>
            {project.sessions.length} {plural(project.sessions.length, "session")}
          </span>
          <span>{project.requests.toLocaleString()} fetches</span>
          <span>
            {project.origins.length} {plural(project.origins.length, "origin")}
          </span>
          {project.findings > 0 ? (
            <span>
              {project.findings} {plural(project.findings, "finding")}
            </span>
          ) : null}
          <span>{ago(project.lastAt)}</span>
        </div>
      </div>
      <div className="work-body">
        <div className="scroll pad">
      <div className="section-head">
        <h3>Scope</h3>
      </div>
      {project.scopes.length === 0 ? (
        <Note>
          No session under this project resolved a scope file, so <code>--project</code> here is only a label.
          Write <code>~/.config/h5i/projects/{project.name}.toml</code> to have the engine hold the line.
        </Note>
      ) : project.scopes.length === 1 ? (
        <p className="count mono">{project.scopes[0]}</p>
      ) : (
        <Note>
          {project.scopes.length} different scopes were in force across these sessions. A result gathered under
          one is not comparable with a result gathered under another.
        </Note>
      )}

      <div className="section-head">
        <h3>Origins reached</h3>
      </div>
      <div className="chips">
        {project.origins.map((o) => (
          <Chip key={o} tone="info">
            {o}
          </Chip>
        ))}
      </div>

      <div className="section-head">
        <h3>Sessions</h3>
      </div>
      <div className="plist">
        {project.sessions.map((s) => (
          <button key={s.id} type="button" className="srow" onClick={() => go(["sessions", s.id])}>
            <div className="srow-top">
              <span className="srow-name">
                {s.name ?? s.id}
                {s.name ? <code>{s.id}</code> : null}
              </span>
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

      <div className="section-head">
        <h3>Fold the rest yourself</h3>
      </div>
        <Cmd
          text={`h5i browser list --all --json | jq '[.[] | select(.project == "${project.name}")]'`}
          hint="every session under this project, including the ones past the console's fold"
        />
        </div>
      </div>
    </div>
  );
}
