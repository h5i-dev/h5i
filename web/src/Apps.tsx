import { useEffect, useMemo, useState } from "react";

import type { Fleet } from "./App";
import {
  api,
  type AppDetail,
  type AppSummary,
  type AppTheorem,
  type MutantView,
  type SpecItem,
} from "./api";
import { Chip, Cmd, Count, Empty, Facts, Note, SectionHead, Spin, Split, type Tone, ago, plural } from "./ui";

// Apps: the Lean proof projects of this repository. The page says what is
// proven, what the proofs take as given, and what the last `cargo app-verify`
// receipt recorded. It never says "verified": a theorem is about the extracted
// kernel, and whether it applies to a running server is a list of checks a
// person closes, not a badge.

const REFRESH_MS = 60_000;

type Tab = "spec" | "theorems" | "trust" | "evidence" | "counterexamples";

const TABS: { key: Tab; label: string }[] = [
  { key: "spec", label: "Spec" },
  { key: "theorems", label: "Theorems" },
  { key: "trust", label: "Trust boundary" },
  { key: "evidence", label: "Evidence" },
  { key: "counterexamples", label: "Counterexamples" },
];

const BAD = new Set(["lean build failed", "axiom gate", "mutant survived", "extraction drift", "extraction failed"]);
const WARN = new Set(["kernel changed since", "proofs changed since", "receipt from a dirty tree", "lean skipped"]);

function flagTone(flag: string): Tone {
  if (BAD.has(flag)) return "bad";
  if (WARN.has(flag)) return "warn";
  return "dim";
}

function worst(flags: string[]): Tone {
  if (flags.some((f) => BAD.has(f))) return "bad";
  if (flags.some((f) => WARN.has(f))) return "warn";
  return flags.length ? "dim" : "plain";
}

function short(hash: string | null | undefined): string {
  return hash ? hash.slice(0, 10) : "";
}

export function AppsPage({
  fleet,
  route,
  go,
}: {
  fleet: Fleet;
  route: string[];
  go: (parts: string[], replace?: boolean) => void;
}) {
  const [apps, setApps] = useState<AppSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);
  const selected = route.length ? route.join("/") : null;
  void fleet;

  // Digests are recomputed on every read, so this list polls slowly and the
  // fleet tick is left alone.
  useEffect(() => {
    const t = window.setInterval(() => setEpoch((e) => e + 1), REFRESH_MS);
    return () => window.clearInterval(t);
  }, []);
  useEffect(() => {
    let live = true;
    api
      .apps()
      .then((a) => {
        if (!live) return;
        setApps(a);
        setError(null);
      })
      .catch((e) => live && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [epoch]);

  const withReceipt = apps?.filter((a) => a.receipt).length ?? 0;
  const flagged = apps?.filter((a) => a.flags.some((f) => BAD.has(f))).length ?? 0;

  if (apps && apps.length === 0) {
    return (
      <>
        <Topbar apps={apps} withReceipt={withReceipt} flagged={flagged} />
        <div className="page">
          <Empty title="No proof projects in this repository">
            <p>
              A proof project is a directory with a <code>lakefile.lean</code> and hand-written Lean next to it, the
              way <code>examples/app/*/proofs</code> are laid out. The console looks for them under the repository
              it was started in.
            </p>
            <Cmd text="cargo app-verify" hint="runs every check and writes .h5i/app-verify/latest.json" />
          </Empty>
        </div>
      </>
    );
  }

  return (
    <>
      <Topbar apps={apps} withReceipt={withReceipt} flagged={flagged} />
      <div className="page">
        <Split
          id="apps"
          initial={340}
          first={
            <div className="column">
              <div className="column-body">
                {error ? <Note tone="bad">{error}</Note> : null}
                {apps === null ? <Spin label="reading proof projects" /> : null}
                {(apps ?? []).map((a) => (
                  <button
                    key={a.dir}
                    type="button"
                    className={`srow${a.dir === selected ? " is-on" : ""}`}
                    onClick={() => go(["apps", ...a.dir.split("/")])}
                  >
                    <div className="srow-top">
                      <span className="srow-name">{a.app}</span>
                      <span className="srow-age">{a.receipt ? ago(a.receipt.started) : "no receipt"}</span>
                    </div>
                    <div className="srow-target">
                      {a.title || a.dir}
                    </div>
                    <div className="srow-bottom">
                      <div className="srow-counts">
                        <Chip tone="dim">{a.kind}</Chip>
                        {a.theorems > 0 ? <Count n={a.theorems} label={plural(a.theorems, "theorem")} /> : null}
                        {a.counterexamples > 0 ? (
                          <Count n={a.counterexamples} label={plural(a.counterexamples, "counterexample")} tone="violet" />
                        ) : null}
                        {a.receipt && a.receipt.mutants.prepared > 0 ? (
                          <Count
                            n={a.receipt.mutants.caught}
                            label={`of ${a.receipt.mutants.prepared} mutants caught`}
                            tone={a.receipt.mutants.survived > 0 ? "bad" : undefined}
                          />
                        ) : null}
                      </div>
                      {a.flags.length > 0 ? <Chip tone={worst(a.flags)}>{a.flags[0]}{a.flags.length > 1 ? ` +${a.flags.length - 1}` : ""}</Chip> : null}
                    </div>
                  </button>
                ))}
              </div>
            </div>
          }
          second={
            selected ? (
              <AppPane dir={selected} epoch={epoch} />
            ) : (
              <Empty title="Select a proof project">
                <p>
                  Each one shows its spec as a reviewer reads it, the theorems and what each rests on, the edges the
                  proofs trust, and the receipts of the last verification run.
                </p>
              </Empty>
            )
          }
        />
      </div>
    </>
  );
}

function Topbar({ apps, withReceipt, flagged }: { apps: AppSummary[] | null; withReceipt: number; flagged: number }) {
  return (
    <div className="topbar">
      <span className="topbar-title">Apps</span>
      <span className="topbar-scope">
        Lean proofs in this repository: what is proven, what is assumed, and what is unconfirmed before a theorem applies to a
        running app
      </span>
      <div className="topbar-right">
        <span className="vital">
          <b>{apps?.length ?? 0}</b> proof projects
        </span>
        <span className="vital">
          <b>{withReceipt}</b> with a receipt
        </span>
        <span className={`vital${flagged > 0 ? " is-loud" : ""}`}>
          <b>{flagged}</b> reporting a failure
        </span>
      </div>
    </div>
  );
}

function AppPane({ dir, epoch }: { dir: string; epoch: number }) {
  const [detail, setDetail] = useState<AppDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("spec");

  useEffect(() => {
    let live = true;
    setDetail(null);
    setErr(null);
    api
      .app(dir)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setErr(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [dir, epoch]);

  if (err) {
    return (
      <div className="work">
        <div className="work-body">
          <Empty title="Could not read this proof project">
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
            <Spin label="reading proofs and the receipt" />
          </div>
        </div>
      </div>
    );
  }

  const s = detail.summary;
  const counterexamples = detail.theorems.filter((t) => t.counterexample);
  const counts: Record<Tab, number | null> = {
    spec: detail.spec ? detail.spec.sections.reduce((n, x) => n + x.items.length, 0) : null,
    theorems: detail.theorems.length - counterexamples.length,
    trust: detail.trust.assumes.length + detail.trust.edges.length + detail.trust.trusted_inputs.length,
    evidence: detail.evidence.steps.length,
    counterexamples: counterexamples.length,
  };

  return (
    <div className="work">
      <div className="work-head">
        <div className="work-title">
          <h2>{s.app}</h2>
          <Chip tone="dim">{s.kind}</Chip>
          {s.flags.map((f) => (
            <Chip key={f} tone={flagTone(f)}>
              {f}
            </Chip>
          ))}
        </div>
        <div className="work-sub">
          <span className="mono">{s.dir}</span>
          {s.title ? <span>{s.title}</span> : null}
          {s.receipt ? (
            <span title={`cargo app-verify at ${s.receipt.started} on ${s.receipt.branch} ${s.receipt.head}`}>
              receipt {ago(s.receipt.started)} at {short(s.receipt.head)}
              {s.receipt.dirty ? " (dirty tree)" : ""}
            </span>
          ) : (
            <span>no receipt yet</span>
          )}
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
              {counts[t.key] !== null ? <span className="tab-n">{counts[t.key]}</span> : null}
            </button>
          ))}
        </div>
      </div>
      <div className="work-body">
        {tab === "spec" ? (
          <SpecTab detail={detail} />
        ) : tab === "theorems" ? (
          <TheoremsTab detail={detail} theorems={detail.theorems.filter((t) => !t.counterexample)} />
        ) : tab === "trust" ? (
          <TrustTab detail={detail} />
        ) : tab === "evidence" ? (
          <EvidenceTab detail={detail} />
        ) : (
          <CounterexamplesTab detail={detail} theorems={counterexamples} />
        )}
      </div>
    </div>
  );
}

// ── Spec: the file a reviewer reads, with the teeth of each clause ───────────

function mutantIndex(detail: AppDetail): Map<string, MutantView> {
  return new Map(detail.evidence.mutants.map((m) => [m.name, m]));
}

function SpecTab({ detail }: { detail: AppDetail }) {
  const spec = detail.spec;
  const mutants = useMemo(() => mutantIndex(detail), [detail]);
  const prepared = detail.evidence.counts.prepared;
  if (!spec) {
    return (
      <div className="scroll pad">
        <Empty title="No Spec.lean">
          <p>
            This project has no <code>Spec.lean</code> to outline. The theorems tab lists what its README states.
          </p>
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      <Note>
        <code>{detail.readme ? `${spec.file}` : spec.file}</code> as a reviewer reads it. The policy and the
        invariants are the requirement; the theorems say the kernel meets them. Beside each clause: the injected
        bugs whose failing proof named it. {prepared > 0
          ? `${prepared} ${plural(prepared, "mutant was", "mutants were")} prepared; a clause no mutant reached is unevaluated, not weak.`
          : "No mutation run is on record, so no clause has been exercised yet."}
      </Note>
      {spec.sections.map((sec, i) => (
        <section key={i} className="spec-section">
          {sec.heading ? <SectionHead>{sec.heading}</SectionHead> : null}
          {sec.items.map((item) => (
            <SpecItemView key={`${item.name}-${item.line}`} item={item} mutants={mutants} prepared={prepared} />
          ))}
        </section>
      ))}
    </div>
  );
}

function SpecItemView({ item, mutants, prepared }: { item: SpecItem; mutants: Map<string, MutantView>; prepared: number }) {
  const [open, setOpen] = useState(item.fields.length === 0 && !item.matrix && item.code.split("\n").length <= 12);
  return (
    <article className="spec-item">
      <div className="spec-item-head">
        <Chip tone="dim">{item.kind}</Chip>
        <code className="spec-name">{item.name}</code>
        <span className="spec-line">line {item.line}</span>
      </div>
      {item.doc ? <p className="spec-doc">{item.doc}</p> : null}
      {item.matrix ? <MatrixView m={item.matrix} /> : null}
      {item.fields.length > 0 ? (
        <ul className="clauses">
          {item.fields.map((f) => (
            <li key={f.name} className="clause">
              <div className="clause-head">
                <code>{f.name}</code>
                {f.mutants.length > 0 ? (
                  <Chip tone="info" title={f.mutants.map((m) => mutants.get(m)?.bug || m).join("\n")}>
                    caught {f.mutants.length} {plural(f.mutants.length, "injected bug")}
                  </Chip>
                ) : prepared > 0 ? (
                  <Chip tone="dim" title="no prepared mutant's failing proof named this clause">
                    unevaluated
                  </Chip>
                ) : null}
              </div>
              {f.doc ? <p className="clause-doc">{f.doc}</p> : null}
              <code className="clause-prop">{f.prop}</code>
              {f.mutants.length > 0 ? (
                <ul className="clause-bugs">
                  {f.mutants.map((m) => (
                    <li key={m}>
                      <code>{m}</code>
                      {mutants.get(m)?.bug ? `: ${mutants.get(m)?.bug}` : ""}
                    </li>
                  ))}
                </ul>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      {item.code && (item.fields.length === 0 || open) ? (
        <>
          {!open ? (
            <button type="button" className="linklike" onClick={() => setOpen(true)}>
              source
            </button>
          ) : (
            <pre className="spec-code">{item.code}{item.truncated ? "\n…" : ""}</pre>
          )}
        </>
      ) : item.code ? (
        <button type="button" className="linklike" onClick={() => setOpen(true)}>
          source
        </button>
      ) : null}
    </article>
  );
}

function MatrixView({ m }: { m: { rows: string[]; cols: string[]; cells: string[][] } }) {
  return (
    <table className="matrix">
      <thead>
        <tr>
          <th />
          {m.cols.map((c) => (
            <th key={c}>{c}</th>
          ))}
        </tr>
      </thead>
      <tbody>
        {m.rows.map((r, i) => (
          <tr key={r}>
            <th>{r}</th>
            {m.cells[i].map((v, j) => (
              <td key={j} className={v === "true" ? "is-yes" : v === "false" ? "is-no" : ""} title={`${r}, ${m.cols[j]}: ${v || "no arm"}`}>
                {v === "true" ? "allowed" : v === "false" ? "·" : v}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

// ── Theorems ─────────────────────────────────────────────────────────────────

function axiomChip(t: AppTheorem) {
  if (!t.axioms) return <Chip tone="dim" title="not in the axiom gate's list">not gated</Chip>;
  const a = t.axioms;
  if (a.verdict === "ok") return <Chip tone="plain" title={a.axioms.join(", ") || "no axioms"}>{a.axioms.length ? `${a.axioms.length} standard axioms` : "no axioms"}</Chip>;
  if (a.verdict === "missing") return <Chip tone="bad" title="the gate found no axiom report">no report</Chip>;
  return <Chip tone="bad" title={a.axioms.join(", ")}>non-standard axiom</Chip>;
}

function TheoremsTab({ detail, theorems }: { detail: AppDetail; theorems: AppTheorem[] }) {
  const mutants = useMemo(() => mutantIndex(detail), [detail]);
  const assumes = detail.trust.assumes.map((a) => a.id);
  if (theorems.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No theorem table">
          <p>The README beside these proofs has no <code>## Theorems</code> table to list.</p>
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      <Note>
        Each row is a statement about the extracted kernel.{" "}
        {assumes.length > 0
          ? `Before it says anything about a running app, ${assumes.join(", ")} must hold (the trust boundary tab).`
          : "No scope.toml names the assumptions it rests on."}{" "}
        "Expected" is the mutation suite's prediction; "caught" is which injected bugs broke this proof.
      </Note>
      <table className="ptable theorems">
        <thead>
          <tr>
            <th>theorem</th>
            <th>statement</th>
            <th>axioms</th>
            <th>mutants</th>
          </tr>
        </thead>
        <tbody>
          {theorems.map((t) => (
            <tr key={t.name}>
              <td>
                <code title={t.qualified ?? undefined}>{t.name}</code>
                {t.file ? (
                  <div className="subtle">
                    {t.file}:{t.line}
                  </div>
                ) : (
                  <div className="subtle">not found in the proofs</div>
                )}
              </td>
              <td>
                {t.statement || t.doc || <span className="subtle">no statement recorded</span>}
                {t.statement && t.doc && t.doc !== t.statement ? <div className="subtle">{t.doc}</div> : null}
              </td>
              <td>{axiomChip(t)}</td>
              <td>
                <div className="chips">
                  {t.caught.map((m) => (
                    <Chip key={m} tone="info" mono title={mutants.get(m)?.bug || m}>
                      {m}
                    </Chip>
                  ))}
                  {t.expected_by
                    .filter((m) => !t.caught.includes(m))
                    .map((m) => (
                      <Chip key={m} tone={mutants.get(m)?.verdict === "caught" ? "warn" : "bad"} mono title={`expected to break this proof; ${mutants.get(m)?.verdict === "caught" ? "caught by another theorem" : mutants.get(m)?.verdict || "not run"}`}>
                        {m}
                      </Chip>
                    ))}
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ── Trust boundary: what must hold before a theorem applies ──────────────────

function TrustTab({ detail }: { detail: AppDetail }) {
  const t = detail.trust;
  const v = t.version;
  const project = "<project>";
  const matchChip = (m: boolean | null, what: string) =>
    m === null ? (
      <Chip tone="dim">{what}: nothing to compare</Chip>
    ) : m ? (
      <Chip tone="plain">{what} unchanged since the receipt</Chip>
    ) : (
      <Chip tone="warn">{what} changed since the receipt</Chip>
    );
  return (
    <div className="scroll pad">
      <Note>
        Everything below is where the guarantee stops. A finding that lands on one of these is not a contradiction of a
        theorem; it is the theorem's hypothesis failing. Track them in a project so each is closed by a person:
      </Note>
      <Cmd text={`h5i project checklist import -p ${project} --proofs ${detail.summary.dir} --required`} block />

      <SectionHead>Assumptions the theorems rest on</SectionHead>
      {t.assumes.length === 0 ? (
        <p className="subtle">
          No <code>scope.toml</code> in {detail.summary.dir} lists the ledger entries this app depends on.
        </p>
      ) : (
        <table className="ptable">
          <tbody>
            {t.assumes.map((a) => (
              <tr key={a.id}>
                <td className="mono">{a.id}</td>
                <td>{a.text || <span className="subtle">not in the ledger</span>}</td>
                <td className="subtle">{a.today}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {t.trusted_inputs.length > 0 ? (
        <>
          <SectionHead>Shell inputs the kernel takes as true</SectionHead>
          <table className="ptable">
            <tbody>
              {t.trusted_inputs.map((i) => (
                <tr key={i.name}>
                  <td className="mono">{i.name}</td>
                  <td>{i.from}</td>
                  <td className="subtle">{i.note}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : null}

      <SectionHead>Routes and fields outside the kernel's check</SectionHead>
      {t.edges.length === 0 ? (
        <p className="subtle">
          No <code>h5i-allow</code> opt-out and no <code>assume_authenticated</code> in this app's sources.
        </p>
      ) : (
        <table className="ptable">
          <tbody>
            {t.edges.map((e, i) => (
              <tr key={i}>
                <td>
                  <Chip tone="warn">{e.kind}</Chip>
                </td>
                <td className="mono">
                  {e.file}:{e.line}
                </td>
                <td>
                  <code>{e.text}</code>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {t.out_of_scope.length > 0 ? (
        <>
          <SectionHead>Out of scope by design</SectionHead>
          <table className="ptable">
            <tbody>
              {t.out_of_scope.map((o, i) => (
                <tr key={i}>
                  <td>{o.item}</td>
                  <td className="subtle">{o.reason}</td>
                  <td className="subtle mono">{o.ref}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : null}

      <SectionHead>Version: is the code under test the proven code</SectionHead>
      <div className="chips">
        {matchChip(v.kernel_match, "kernel")}
        {matchChip(v.proofs_match, "proofs")}
        {v.receipt_head && v.current_head ? (
          v.receipt_head === v.current_head ? (
            <Chip tone="plain">HEAD is the receipt's commit</Chip>
          ) : (
            <Chip tone="warn">HEAD moved since the receipt</Chip>
          )
        ) : null}
        {v.receipt_dirty ? <Chip tone="warn">receipt from a dirty tree</Chip> : null}
      </div>
      <Facts
        rows={[
          ["receipt commit", v.receipt_head ? <code>{short(v.receipt_head)}</code> : "no receipt"],
          ["current HEAD", v.current_head ? <code>{short(v.current_head)}</code> : "unknown"],
          ["kernel digest now", v.kernel_digest ? <code>{short(v.kernel_digest)}</code> : "no kernel"],
          ["proofs digest now", <code>{short(v.proofs_digest)}</code>],
        ]}
      />
      <p className="subtle">
        The extraction check in the receipt ties the kernel to the generated Lean at that commit. Whether the server a
        finding was taken against was built from that kernel is a separate fact.{" "}
        {v.builds.length === 0
          ? "No box on this repository has a receipt of building this app, so that fact is unconfirmed here."
          : "Builds recorded inside this repository's boxes:"}
      </p>
      {v.builds.length > 0 ? (
        <table className="ptable">
          <tbody>
            {v.builds.map((b, i) => (
              <tr key={i}>
                <td className="mono">{b.env_id}</td>
                <td className="subtle">{ago(b.at)}</td>
                <td>
                  <code>{b.cmd}</code>
                </td>
                <td className="subtle mono">{short(b.git_tree)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </div>
  );
}

// ── Evidence: the receipt, counted ───────────────────────────────────────────

function outcomeTone(o: string): Tone {
  return o === "pass" ? "plain" : o === "fail" ? "bad" : "dim";
}

function verdictTone(v: string): Tone {
  return v === "caught" ? "plain" : v === "survived" ? "bad" : v === "baseline-ok" ? "dim" : "warn";
}

function EvidenceTab({ detail }: { detail: AppDetail }) {
  const e = detail.evidence;
  const r = detail.summary.receipt;
  const [showPassed, setShowPassed] = useState(false);
  if (!r) {
    return (
      <div className="scroll pad">
        <Empty title="No verification receipt">
          <p>
            <code>cargo app-verify</code> runs the route and command checks, extraction drift, every proof project,
            the axiom gates and, with <code>--full</code>, the mutation suites and the differential test. It writes
            what it found to <code>.h5i/app-verify/latest.json</code>; this tab reads that file.
          </p>
          <Cmd text="cargo app-verify --full" />
        </Empty>
      </div>
    );
  }
  const steps = showPassed ? e.steps : e.steps.filter((s) => s.outcome !== "pass");
  return (
    <div className="scroll pad">
      <div className="chips">
        <Chip tone={e.summary && e.summary.failed > 0 ? "bad" : "plain"}>
          {e.summary?.passed ?? 0} passed, {e.summary?.failed ?? 0} failed, {e.summary?.skipped ?? 0} skipped
        </Chip>
        <Chip tone={outcomeTone(r.lean)}>lean {r.lean}</Chip>
        <Chip tone={r.extraction === "match" ? "plain" : r.extraction === "none" || r.extraction === "skip" ? "dim" : "bad"}>
          extraction {r.extraction === "none" || r.extraction === "skip" ? "not run" : r.extraction}
        </Chip>
        {r.authorization ? (
          <Chip tone={r.authorization === "none" ? "warn" : "plain"} title="states a universal authorization theorem">
            authorization {r.authorization}
          </Chip>
        ) : null}
        <Chip tone={r.axioms.bad + r.axioms.missing > 0 ? "bad" : "plain"}>
          axioms {r.axioms.ok} ok{r.axioms.bad ? `, ${r.axioms.bad} bad` : ""}
          {r.axioms.missing ? `, ${r.axioms.missing} missing` : ""}
        </Chip>
        {!r.full ? <Chip tone="dim">no --full: mutants and difftest not run</Chip> : null}
      </div>
      <p className="subtle">
        Run {ago(r.started)} on <code>{r.branch}</code> at <code>{short(r.head)}</code>
        {r.dirty ? ", with uncommitted changes" : ""}. These are counts over one run, not a grade of the proofs.
      </p>

      <SectionHead
        aside={
          <button type="button" className="linklike" onClick={() => setShowPassed(!showPassed)}>
            {showPassed ? "hide passed" : `show ${e.steps.filter((s) => s.outcome === "pass").length} passed`}
          </button>
        }
      >
        Steps
      </SectionHead>
      {steps.length === 0 ? (
        <p className="subtle">Every step of this run passed.</p>
      ) : (
        <table className="ptable">
          <tbody>
            {steps.map((s, i) => (
              <tr key={i}>
                <td>{s.name}</td>
                <td>
                  <Chip tone={outcomeTone(s.outcome)}>{s.outcome}</Chip>
                </td>
                <td className="subtle">{s.secs >= 0.1 ? `${s.secs.toFixed(1)}s` : ""}</td>
                <td className="subtle">{s.outcome === "fail" ? <pre className="step-log">{s.detail}</pre> : s.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <SectionHead>Mutants</SectionHead>
      {e.counts.prepared === 0 ? (
        <p className="subtle">
          No mutation record for this project in the receipt.{" "}
          <Cmd text="cargo app-verify --full" hint="adds the mutation suites and the differential test" />
        </p>
      ) : (
        <>
          <p className="subtle">
            {e.counts.caught} of {e.counts.prepared} prepared bugs broke a proof
            {e.counts.survived ? `; ${e.counts.survived} survived, which means the spec did not notice them` : ""}
            {e.counts.invalid ? `; ${e.counts.invalid} did not compile or extract and count for nothing` : ""}. The
            number says what was tried, not how complete the spec is.
          </p>
          <table className="ptable">
            <thead>
              <tr>
                <th>mutant</th>
                <th>injected bug</th>
                <th>verdict</th>
                <th>broke</th>
              </tr>
            </thead>
            <tbody>
              {e.mutants.map((m) => (
                <tr key={m.name}>
                  <td className="mono">{m.name}</td>
                  <td>{m.bug}</td>
                  <td>
                    <div className="chips">
                      <Chip tone={verdictTone(m.verdict)}>{m.verdict}</Chip>
                      {m.as_expected === false ? (
                        <Chip tone="warn" title={`expected ${m.expect.join(", ")}`}>
                          not where expected
                        </Chip>
                      ) : null}
                      {m.verdict === "invalid" ? <Chip tone="dim">{m.stage}</Chip> : null}
                    </div>
                  </td>
                  <td>
                    {m.failed.length > 0 ? (
                      <div className="chips">
                        {m.failed.map((d) => (
                          <Chip key={d} tone="info" mono>
                            {d.split(".").slice(-1)[0]}
                          </Chip>
                        ))}
                      </div>
                    ) : m.verdict === "caught" ? (
                      <span className="subtle">build failed without a located error</span>
                    ) : (
                      ""
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {e.difftest ? (
        <>
          <SectionHead>Rust against Lean</SectionHead>
          <p className="subtle">
            {e.difftest.cases.toLocaleString()} random cases agreed between the Rust kernel and the extracted Lean.
            Outcome mix: <code>{e.difftest.outcomes}</code>.
          </p>
        </>
      ) : null}
    </div>
  );
}

// ── Counterexamples: proofs that a bug exists ────────────────────────────────

function CounterexamplesTab({ detail, theorems }: { detail: AppDetail; theorems: AppTheorem[] }) {
  if (theorems.length === 0) {
    return (
      <div className="scroll pad">
        <Empty title="No counterexample theorems">
          <p>
            A counterexample is a theorem of the form <code>¬ property old_code</code>: Lean refutes a property for the
            code before a fix, or for upstream as it stands. None is named for this project.
          </p>
        </Empty>
      </div>
    );
  }
  return (
    <div className="scroll pad">
      <Note>
        Each is a proven bug with a concrete state. One that is still present upstream is a finding waiting to be
        reproduced on the live app; file it against the theorem so the chain stays visible:
      </Note>
      <Cmd text={`h5i project finding create -p <project> --title '...' --proof-theorem ${theorems[0].name} --proof-relation in-scope --proof-version unconfirmed`} block />
      {theorems.map((t) => (
        <article key={t.name} className="spec-item">
          <div className="spec-item-head">
            <code className="spec-name">{t.name}</code>
            {t.bug?.bug ? <Chip tone="violet">{t.bug.bug}</Chip> : null}
            {t.bug?.current ? <Chip tone="warn">still present upstream</Chip> : t.bug?.before ? <Chip tone="dim">fixed in {t.bug.before}</Chip> : null}
            {t.file ? (
              <span className="spec-line">
                {t.file}:{t.line}
              </span>
            ) : null}
          </div>
          {t.statement ? <p className="spec-doc">{t.statement}</p> : null}
          {t.doc && t.doc !== t.statement ? <p className="subtle">{t.doc}</p> : null}
          {t.bug?.note ? <p className="subtle">{t.bug.note}</p> : null}
        </article>
      ))}
      {detail.scope?.upstream ? (
        <p className="subtle">
          Upstream: <code>{detail.scope.upstream}</code>
          {detail.scope.pinned ? ` at ${detail.scope.pinned}` : ""}
        </p>
      ) : null}
    </div>
  );
}
