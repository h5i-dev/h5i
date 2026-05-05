import { useEffect, useState } from "react";
import { Callout, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import {
  api,
  githubFileUrl,
  type Commit,
  type ContextRelevant,
  type Repo,
} from "./api";

// Per-commit Context tab: shows context entries that mention any file this
// commit touched. We fetch the commit's file list (/api/commit-files), then
// query /api/context/relevant?file=X for each file in parallel and merge.
//
// This is the "killer feature" surface — the only place in the UI where a
// commit's *code change* is cross-linked to the *reasoning* recorded in the
// h5i context workspace.

interface FileGroup {
  file: string;
  hits: { branch: string; source: string; text: string }[];
}

export function CommitContextTab({
  commit,
  repo,
}: {
  commit: Commit;
  repo: Repo | null;
}) {
  const [groups, setGroups] = useState<FileGroup[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setGroups(null);
    setError(null);
    let cancelled = false;

    (async () => {
      try {
        const cf = await api.commitFiles(commit.git_oid);
        if (cancelled) return;
        if (cf.files.length === 0) {
          setGroups([]);
          return;
        }
        const results = await Promise.all(
          cf.files.map(async (file) => {
            try {
              const r = (await api.contextRelevant(file)) as
                | ContextRelevant
                | unknown;
              const hits = normalizeHits(r);
              return hits.length > 0 ? { file, hits } : null;
            } catch {
              return null;
            }
          }),
        );
        if (cancelled) return;
        setGroups(results.filter((g): g is FileGroup => g !== null));
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [commit.git_oid]);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load context" description={error} />
    );
  }
  if (!groups) {
    return (
      <NonIdealState icon={<Spinner size={20} />} title="Searching context…" />
    );
  }
  if (groups.length === 0) {
    return (
      <div style={{ padding: 16 }}>
        <Callout intent="none" icon="info-sign" compact>
          No context entries reference any of the files this commit touched.
          Use <code>h5i context trace</code> while working to record reasoning
          that future workbench sessions can link back here.
        </Callout>
      </div>
    );
  }

  return (
    <>
      {groups.map((g) => {
        const url = githubFileUrl(repo?.github_url, repo?.branch ?? "HEAD", g.file);
        return (
          <div className="wb-detail-section" key={g.file}>
            <div className="wb-detail-title">
              <span style={{ fontFamily: "monospace", textTransform: "none" }}>
                {url ? (
                  <a href={url} target="_blank" rel="noreferrer noopener">
                    {g.file}
                  </a>
                ) : (
                  g.file
                )}
              </span>
              <Tag minimal style={{ marginLeft: 6, fontFamily: "monospace", fontSize: 11 }}>
                {g.hits.length}
              </Tag>
            </div>
            {g.hits.slice(0, 5).map((h, i) => (
              <div
                key={i}
                style={{
                  display: "flex",
                  gap: 8,
                  padding: "6px 0",
                  borderTop: i === 0 ? "none" : "1px solid var(--bp-border)",
                  fontSize: 12,
                }}
              >
                <Tag
                  minimal
                  intent={
                    h.source === "milestone"
                      ? "primary"
                      : h.source === "trace"
                        ? "warning"
                        : undefined
                  }
                  style={{ fontSize: 10, flexShrink: 0 }}
                >
                  {h.source}
                </Tag>
                <span
                  style={{
                    color: "var(--bp-text-muted)",
                    lineHeight: 1.5,
                    overflow: "hidden",
                    display: "-webkit-box",
                    WebkitLineClamp: 3,
                    WebkitBoxOrient: "vertical",
                  }}
                >
                  {h.text}
                </span>
              </div>
            ))}
            {g.hits.length > 5 ? (
              <div
                style={{
                  fontSize: 11,
                  color: "var(--bp-text-dim)",
                  marginTop: 4,
                  fontStyle: "italic",
                }}
              >
                + {g.hits.length - 5} more
              </div>
            ) : null}
          </div>
        );
      })}
    </>
  );
}

// The legacy ctx API can return either an envelope `{file, hits: [...]}` or a
// raw array of strings depending on whether matches were found and which
// version is running. Coerce both into our unified hit list.
function normalizeHits(raw: unknown): {
  branch: string;
  source: string;
  text: string;
}[] {
  if (!raw) return [];
  if (Array.isArray(raw)) {
    return raw.map((s) => ({
      branch: "main",
      source: "ctx",
      text: typeof s === "string" ? s : JSON.stringify(s),
    }));
  }
  if (typeof raw === "object" && raw !== null && "hits" in raw) {
    const hits = (raw as { hits?: unknown }).hits;
    if (Array.isArray(hits)) {
      return hits
        .map((h) => {
          if (typeof h === "string") {
            return { branch: "main", source: "ctx", text: h };
          }
          if (typeof h === "object" && h !== null) {
            const o = h as Record<string, unknown>;
            return {
              branch: typeof o.branch === "string" ? o.branch : "main",
              source: typeof o.source === "string" ? o.source : "ctx",
              text:
                typeof o.text === "string"
                  ? o.text
                  : typeof o.snippet === "string"
                    ? o.snippet
                    : JSON.stringify(h),
            };
          }
          return null;
        })
        .filter((h): h is { branch: string; source: string; text: string } => h !== null);
    }
  }
  return [];
}
