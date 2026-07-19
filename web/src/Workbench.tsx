import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Button,
  ButtonGroup,
  HTMLTable,
  InputGroup,
  Menu,
  MenuDivider,
  MenuItem,
  NonIdealState,
  Popover,
  Spinner,
  Tab,
  Tabs,
  Tag,
} from "@blueprintjs/core";

import {
  api,
  githubBranchUrl,
  githubCommitUrl,
  type Commit,
  type Repo,
} from "./api";
import { CommitDetail } from "./CommitDetail";
import { RefsTab } from "./RefsTab";
import { SessionsTab } from "./SessionsTab";
import { IntegrityTab } from "./IntegrityTab";
import { ContextView } from "./ContextView";
import { CommitContextTab } from "./CommitContextTab";
import { MemoryView } from "./MemoryView";
import { ReplayView } from "./ReplayView";
import { CockpitView } from "./CockpitView";
import { RadioView } from "./RadioView";
import { SandboxView } from "./SandboxView";
import { BoardView } from "./BoardView";
import { TeamView } from "./TeamView";
import { ContextStrip } from "./ContextStrip";
import { BranchPicker } from "./BranchPicker";
import { AttentionRail } from "./AttentionRail";
import {
  getAttention,
  subscribeUpdates,
  type AttentionReport,
  type EntityRef,
  type WorkItem,
} from "./attention";

/** Priorities that mean "blocked on a human" — the notification set. */
const NEEDS_YOU = new Set(["critical", "decision", "communication"]);

type Mode = "replay" | "cockpit" | "radio" | "team" | "explore" | "memory" | "context" | "sandbox" | "board";
type RightTab = "refs" | "sessions" | "integrity" | "context";

export function Workbench() {
  const [repo, setRepo] = useState<Repo | null>(null);
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedOid, setSelectedOid] = useState<string | null>(null);
  // Default landing is Sandbox — the fleet of isolated envs (what is running,
  // under which policy, with what evidence) is the first thing an operator
  // wants to see. Users jump to Ensemble and the other views via the header nav.
  const [mode, setMode] = useState<Mode>("sandbox");
  const [rightTab, setRightTab] = useState<RightTab>("refs");
  // When the cockpit asks to replay a specific commit, focus the replay there.
  const [replayFocusOid, setReplayFocusOid] = useState<string | null>(null);
  // null = follow HEAD (server default); a string = explicit branch override.
  const [activeBranch, setActiveBranch] = useState<string | null>(null);

  // ── attention: the live triage projection + entity focus (deep links) ──
  const [attention, setAttention] = useState<AttentionReport | null>(null);
  const [sseConnected, setSseConnected] = useState(false);
  const [focusEnv, setFocusEnv] = useState<string | null>(null);
  const [focusTeam, setFocusTeam] = useState<string | null>(null);
  const notifiedRef = useRef<Set<string>>(new Set());

  const refreshAttention = useCallback(() => {
    getAttention().then(setAttention).catch(() => {});
  }, []);

  // One SSE subscription feeds the rail (and everything else that wants
  // liveness) — no view polls for attention state.
  useEffect(() => {
    refreshAttention();
    return subscribeUpdates((u) => {
      if (u.attention) setAttention(u.attention);
    }, setSseConnected);
  }, [refreshAttention]);

  // Browser notifications only on blocked-on-you transitions, keyed by
  // (id, watermark) so a re-armed item notifies again but nothing repeats.
  useEffect(() => {
    if (!attention || typeof Notification === "undefined") return;
    if (Notification.permission !== "granted") return;
    for (const item of attention.items) {
      const key = `${item.id}@${item.occurred_at}`;
      if (item.seen_at || !NEEDS_YOU.has(item.priority)) continue;
      if (notifiedRef.current.has(key)) continue;
      notifiedRef.current.add(key);
      new Notification(`h5i · ${item.title}`, { body: item.reasons[0] ?? "" });
    }
  }, [attention]);

  // ── addressable URLs: #/env/<agent>/<slug>, #/team/<id>, #/commit/<oid>,
  // #/mode/<mode>. The hash is the single source of navigation truth, so
  // back/forward work and every view is shareable.
  const applyHash = useCallback(() => {
    const parts = window.location.hash.replace(/^#\/?/, "").split("/").filter(Boolean);
    if (parts[0] === "env" && parts.length >= 3) {
      setFocusEnv(`env/${parts[1]}/${parts[2]}`);
      setMode("sandbox");
    } else if (parts[0] === "team" && parts[1]) {
      setFocusTeam(decodeURIComponent(parts[1]));
      setMode("team");
    } else if (parts[0] === "commit" && parts[1]) {
      setSelectedOid(parts[1]);
      setMode("explore");
    } else if (parts[0] === "mode" && parts[1]) {
      setMode(parts[1] as Mode);
    }
  }, []);

  useEffect(() => {
    applyHash();
    window.addEventListener("hashchange", applyHash);
    return () => window.removeEventListener("hashchange", applyHash);
  }, [applyHash]);

  const navigate = useCallback(
    (hash: string) => {
      if (window.location.hash === `#${hash}`) applyHash();
      else window.location.hash = hash;
    },
    [applyHash],
  );

  const openEntity = useCallback(
    (entity: EntityRef) => {
      if (entity.kind === "env") navigate(`/${entity.id}`);
      else if (entity.kind === "team") navigate(`/team/${encodeURIComponent(entity.id)}`);
      else if (entity.kind === "msg") navigate("/mode/radio");
    },
    [navigate],
  );

  const loadRepo = useCallback(() => {
    api.repo().then(setRepo).catch((e) => setError(String(e)));
  }, []);

  const loadCommits = useCallback(
    (branch: string | null) => {
      setError(null);
      setCommits(null);
      api
        // Scope Explore to the branch's own commits (base..branch); the default
        // branch has no base and falls back to a full walk.
        .commits({ limit: 200, branch, branchOnly: !!branch })
        .then((cs) => {
          setCommits(cs);
          // When the branch changes, prefer the new tip; otherwise keep current
          // selection if it's still in the list.
          setSelectedOid((prev) => {
            if (cs.length === 0) return null;
            if (prev && cs.some((c) => c.git_oid === prev)) return prev;
            return cs[0].git_oid;
          });
        })
        .catch((e) => setError(String(e)));
    },
    [],
  );

  // Resolve the branch the whole workbench follows: an explicit picker override,
  // else the repo's current branch. Commit logs (Explore) and the context
  // dashboard both scope to this.
  const branchInUI = activeBranch ?? repo?.branch ?? null;

  useEffect(() => {
    loadRepo();
  }, [loadRepo]);
  useEffect(() => {
    loadCommits(branchInUI);
  }, [branchInUI, loadCommits]);

  const refresh = () => {
    loadRepo();
    loadCommits(branchInUI);
  };

  const selected = useMemo(
    () => commits?.find((c) => c.git_oid === selectedOid) ?? null,
    [commits, selectedOid],
  );

  const jumpToCommit = (oid: string) => {
    navigate(`/commit/${oid}`);
  };
  const ghBranchUrl = repo?.github_url
    ? (b: string) => githubBranchUrl(repo.github_url, b)
    : null;

  return (
    <div className="wb-shell">
      <header className="wb-header">
        <div className="wb-header-left">
          <div className="wb-brand-lockup">
            <span className="wb-brand">h5i</span>
            <span className="wb-brand-eyebrow">agent ensemble</span>
          </div>
          <span className="wb-header-sep">/</span>
          <span className="wb-repo">{repo?.name ?? "—"}</span>
          {branchInUI ? (
            <BranchPicker
              current={branchInUI}
              onChange={(name) => setActiveBranch(name)}
              githubBranchUrl={ghBranchUrl}
            />
          ) : null}
        </div>

        <nav className="wb-header-modes">
          <ButtonGroup minimal>
            <Button
              className="wb-mode-lead"
              icon="shield"
              text="Sandbox"
              active={mode === "sandbox"}
              onClick={() => navigate("/mode/sandbox")}
            />
            <Button
              icon="grid-view"
              text="Board"
              active={mode === "board"}
              onClick={() => navigate("/mode/board")}
            />
            <Button
              icon="endorsed"
              text="Cockpit"
              active={mode === "cockpit"}
              onClick={() => navigate("/mode/cockpit")}
            />
            <Button
              icon="feed"
              text="Radio"
              active={mode === "radio"}
              onClick={() => navigate("/mode/radio")}
            />
            <Button
              icon="people"
              text="Ensemble"
              active={mode === "team"}
              onClick={() => navigate("/mode/team")}
            />
            <Button
              icon="lightbulb"
              text="Context"
              active={mode === "context"}
              onClick={() => navigate("/mode/context")}
            />
            <Button
              icon="play"
              text="Replay"
              active={mode === "replay"}
              onClick={() => navigate("/mode/replay")}
            />
            <Button
              icon="search-around"
              text="Explore"
              active={mode === "explore"}
              onClick={() => navigate("/mode/explore")}
            />
            <Button
              icon="database"
              text="Memory"
              active={mode === "memory"}
              onClick={() => navigate("/mode/memory")}
            />
          </ButtonGroup>
        </nav>

        <div className="wb-header-right">
          <QuickSearch
            commits={commits}
            onSelectCommit={jumpToCommit}
            onSelectBranch={(name) => {
              setActiveBranch(name);
              navigate("/mode/explore");
            }}
            onOpenContext={() => navigate("/mode/context")}
            workItems={attention?.work_items ?? null}
            onOpenEntity={openEntity}
          />
          {repo?.github_url ? (
            <a
              href={repo.github_url}
              target="_blank"
              rel="noreferrer noopener"
              className="bp5-button bp5-minimal bp5-small"
              title="Open repo on GitHub"
            >
              <span className="bp5-icon bp5-icon-git-repo" aria-hidden />
              <span style={{ marginLeft: 4 }}>GitHub</span>
            </a>
          ) : null}
          <Button
            minimal
            small
            icon="notifications"
            title="Enable desktop notifications when something blocks on you"
            onClick={() => {
              if (typeof Notification !== "undefined") void Notification.requestPermission();
            }}
          />
          <Button minimal small icon="refresh" onClick={refresh} title="Refresh" />
        </div>
      </header>

      <ContextStrip
        repoBranch={branchInUI}
        onOpen={() => navigate("/mode/context")}
      />

      <div className="wb-with-rail">
        <AttentionRail
          report={attention}
          connected={sseConnected}
          onOpen={openEntity}
          onDrained={refreshAttention}
        />
        <div className="wb-rail-main">
      {mode === "replay" ? (
        <ReplayView focusOid={replayFocusOid} branch={branchInUI} />
      ) : mode === "cockpit" ? (
        <CockpitView
          branch={branchInUI}
          onOpenReplay={(oid) => {
            setReplayFocusOid(oid);
            navigate("/mode/replay");
          }}
        />
      ) : mode === "radio" ? (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <RadioView branch={branchInUI} />
          </div>
        </div>
      ) : mode === "team" ? (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <TeamView focusRun={focusTeam} />
          </div>
        </div>
      ) : mode === "explore" ? (
        <div className="wb-body">
          <CommitListPane
            commits={commits}
            error={error}
            selectedOid={selectedOid}
            onSelect={setSelectedOid}
            githubUrl={repo?.github_url ?? null}
          />
          <DetailPane commit={selected} repo={repo} />
          <RightPane
            commit={selected}
            repo={repo}
            tab={rightTab}
            onTabChange={setRightTab}
            onSelect={jumpToCommit}
          />
        </div>
      ) : mode === "memory" ? (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <MemoryView />
          </div>
        </div>
      ) : mode === "sandbox" ? (
        <SandboxView focusEnv={focusEnv} />
      ) : mode === "board" ? (
        <BoardView />
      ) : (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <div className="wb-pane-header">Context workspace</div>
            <div className="wb-pane-body wb-context-body">
              <ContextView branch={branchInUI} />
            </div>
          </div>
        </div>
      )}
        </div>
      </div>

      <StatusBar
        repo={repo}
        commits={commits}
        selected={selected}
        mode={mode}
        branch={branchInUI}
      />
    </div>
  );
}

type SearchHit =
  | { kind: "commit"; id: string; title: string; detail: string; oid: string }
  | { kind: "branch"; id: string; title: string; detail: string; name: string }
  | { kind: "work"; id: string; title: string; detail: string; item: WorkItem }
  | { kind: "command"; id: string; title: string; detail: string; run: () => void };

function QuickSearch({
  commits,
  onSelectCommit,
  onSelectBranch,
  onOpenContext,
  workItems,
  onOpenEntity,
}: {
  commits: Commit[] | null;
  onSelectCommit: (oid: string) => void;
  onSelectBranch: (name: string) => void;
  onOpenContext: () => void;
  workItems: WorkItem[] | null;
  onOpenEntity: (entity: EntityRef) => void;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [branches, setBranches] = useState<string[] | null>(null);

  useEffect(() => {
    if (!open || branches !== null) return;
    api
      .branches()
      .then((bs) => setBranches(bs.filter((b) => !b.is_remote).map((b) => b.name)))
      .catch(() => setBranches([]));
  }, [open, branches]);

  const hits = useMemo<SearchHit[]>(() => {
    const q = query.trim().toLowerCase();
    const commandHits: SearchHit[] = [
      {
        kind: "command",
        id: "context",
        title: "Open Context dashboard",
        detail: "Workspace goal, trace, branches, snapshots",
        run: onOpenContext,
      },
    ];
    if (!q) return commandHits;

    // Work items lead: selecting work is the workbench's primary act.
    const workHits =
      workItems
        ?.filter((w) => `${w.kind} ${w.id} ${w.title} ${w.lifecycle}`.toLowerCase().includes(q))
        .slice(0, 6)
        .map<SearchHit>((w) => ({
          kind: "work",
          id: `work:${w.id}`,
          item: w,
          title: w.title,
          detail: `${w.kind} · ${w.lifecycle}${w.unseen ? ` · ${w.unseen} unseen` : ""}`,
        })) ?? [];

    const commitHits =
      commits
        ?.filter((c) => {
          const haystack = `${c.short_oid} ${c.git_oid} ${c.message} ${c.author} ${c.ai_model ?? ""}`.toLowerCase();
          return haystack.includes(q);
        })
        .slice(0, 7)
        .map<SearchHit>((c) => ({
          kind: "commit",
          id: `commit:${c.git_oid}`,
          oid: c.git_oid,
          title: `${c.short_oid.slice(0, 7)} ${c.message.split("\n")[0]}`,
          detail: `${c.author.split(" ")[0]}${c.ai_model ? " · " + shortModel(c.ai_model) : ""}`,
        })) ?? [];

    const branchHits =
      branches
        ?.filter((b) => b.toLowerCase().includes(q))
        .slice(0, 5)
        .map<SearchHit>((b) => ({
          kind: "branch",
          id: `branch:${b}`,
          name: b,
          title: b,
          detail: "Switch branch",
        })) ?? [];

    return [
      ...workHits,
      ...commitHits,
      ...branchHits,
      ...commandHits.filter((h) => h.title.toLowerCase().includes(q)),
    ];
  }, [branches, commits, onOpenContext, query, workItems]);

  const choose = (hit: SearchHit) => {
    if (hit.kind === "commit") onSelectCommit(hit.oid);
    if (hit.kind === "branch") onSelectBranch(hit.name);
    if (hit.kind === "work")
      onOpenEntity(
        hit.item.kind === "team"
          ? { kind: "team", id: hit.item.id.replace(/^team\//, "") }
          : { kind: "env", id: hit.item.id },
      );
    if (hit.kind === "command") hit.run();
    setQuery("");
    setOpen(false);
  };

  return (
    <Popover
      isOpen={open}
      onInteraction={setOpen}
      placement="bottom-end"
      minimal
      content={
        <Menu className="wb-search-menu">
          <MenuDivider title={query.trim() ? "Results" : "Quick actions"} />
          {hits.length === 0 ? (
            <MenuItem disabled text="No matches" icon="search" />
          ) : (
            hits.map((hit) => (
              <MenuItem
                key={hit.id}
                icon={
                  hit.kind === "commit"
                    ? "git-commit"
                    : hit.kind === "branch"
                      ? "git-branch"
                      : hit.kind === "work"
                        ? hit.item.kind === "team"
                          ? "people"
                          : "shield"
                        : "lightbulb"
                }
                text={
                  <span className="wb-search-hit">
                    <span className="wb-search-hit-title">{hit.title}</span>
                    <span className="wb-search-hit-detail">{hit.detail}</span>
                  </span>
                }
                onClick={() => choose(hit)}
              />
            ))
          )}
        </Menu>
      }
    >
      <InputGroup
        className="wb-search"
        leftIcon="search"
        placeholder="Search commits, branches"
        value={query}
        onFocus={() => setOpen(true)}
        onChange={(e) => {
          setQuery(e.currentTarget.value);
          setOpen(true);
        }}
      />
    </Popover>
  );
}

function CommitListPane(props: {
  commits: Commit[] | null;
  error: string | null;
  selectedOid: string | null;
  onSelect: (oid: string) => void;
  githubUrl: string | null;
}) {
  const { commits, error, selectedOid, onSelect, githubUrl } = props;

  return (
    <div className="wb-pane">
      <div className="wb-pane-header">
        <span>Commits</span>
        {commits ? (
          <Tag minimal round>
            {commits.length}
          </Tag>
        ) : null}
      </div>
      <div className="wb-pane-body">
        {error ? (
          <NonIdealState icon="error" title="Failed to load" description={error} />
        ) : !commits ? (
          <NonIdealState icon={<Spinner size={20} />} title="Loading commits…" />
        ) : commits.length === 0 ? (
          <NonIdealState icon="git-commit" title="No commits" />
        ) : (
          <HTMLTable className="wb-commits-table" interactive compact>
            <thead>
              <tr>
                <th style={{ width: 90 }}>Commit</th>
                <th>Message</th>
                <th style={{ width: 60 }}>Author</th>
                <th style={{ width: 70 }}>Model</th>
              </tr>
            </thead>
            <tbody>
              {commits.map((c) => {
                const ghUrl = githubCommitUrl(githubUrl, c.git_oid);
                return (
                  <tr
                    key={c.git_oid}
                    className={c.git_oid === selectedOid ? "selected" : ""}
                    onClick={() => onSelect(c.git_oid)}
                  >
                    <td>
                      <span className={"wb-oid " + (c.ai_model ? "ai" : "")}>
                        {c.short_oid.slice(0, 7)}
                      </span>
                      {ghUrl ? (
                        <a
                          href={ghUrl}
                          target="_blank"
                          rel="noreferrer noopener"
                          className="wb-oid-gh"
                          onClick={(e) => e.stopPropagation()}
                          title="Open commit on GitHub"
                        >
                          <span className="bp5-icon bp5-icon-share" aria-hidden />
                        </a>
                      ) : null}
                    </td>
                    <td className="wb-msg" title={c.message}>
                      {c.message.split("\n")[0]}
                    </td>
                    <td
                      style={{
                        fontSize: 12,
                        color: "var(--bp-text-muted)",
                        whiteSpace: "nowrap",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        maxWidth: 60,
                      }}
                      title={c.author}
                    >
                      {c.author.split(" ")[0]}
                    </td>
                    <td>
                      {c.ai_model ? (
                        <Tag minimal intent="primary" style={{ fontSize: 11 }}>
                          {shortModel(c.ai_model)}
                        </Tag>
                      ) : (
                        <span style={{ color: "var(--bp-text-dim)", fontSize: 12 }}>
                          —
                        </span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </HTMLTable>
        )}
      </div>
    </div>
  );
}

function DetailPane(props: { commit: Commit | null; repo: Repo | null }) {
  return (
    <div className="wb-pane">
      <div className="wb-pane-header">
        <span>Detail</span>
        {props.commit ? (
          <Tag minimal style={{ fontFamily: "monospace" }}>
            {props.commit.short_oid.slice(0, 12)}
          </Tag>
        ) : null}
      </div>
      <div className="wb-pane-body">
        {props.commit ? (
          <CommitDetail commit={props.commit} repo={props.repo} />
        ) : (
          <div className="wb-pane-empty">Select a commit to inspect.</div>
        )}
      </div>
    </div>
  );
}

function RightPane(props: {
  commit: Commit | null;
  repo: Repo | null;
  tab: RightTab;
  onTabChange: (t: RightTab) => void;
  onSelect: (oid: string) => void;
}) {
  const { commit, repo, tab, onTabChange, onSelect } = props;

  if (!commit) {
    return (
      <div className="wb-pane">
        <div className="wb-pane-header">Cross-reference</div>
        <div className="wb-pane-body">
          <div className="wb-pane-empty">Select a commit to see cross-references.</div>
        </div>
      </div>
    );
  }

  return (
    <div className="wb-pane">
      <div className="wb-pane-header" style={{ padding: 0 }}>
        <Tabs
          id="right-pane-tabs"
          selectedTabId={tab}
          onChange={(id) => onTabChange(id as RightTab)}
          className="wb-right-tabs"
        >
          <Tab id="refs" title="Refs" />
          <Tab id="sessions" title="Sessions" />
          <Tab id="integrity" title="Integrity" />
          <Tab id="context" title="Context" />
        </Tabs>
      </div>
      <div className="wb-pane-body">
        {tab === "refs" ? (
          <RefsTab commit={commit} onSelect={onSelect} />
        ) : tab === "sessions" ? (
          <SessionsTab commit={commit} />
        ) : tab === "integrity" ? (
          <IntegrityTab commit={commit} />
        ) : (
          <CommitContextTab commit={commit} repo={repo} />
        )}
      </div>
    </div>
  );
}

function StatusBar(props: {
  repo: Repo | null;
  commits: Commit[] | null;
  selected: Commit | null;
  mode: Mode;
  branch: string | null;
}) {
  const { repo, commits, selected, mode, branch } = props;
  return (
    <div className="wb-statusbar">
      <span>
        commits:{" "}
        <span style={{ color: "var(--bp-text)" }}>{repo?.total_commits ?? "—"}</span>
      </span>
      <span>
        ai-assisted:{" "}
        <span style={{ color: "var(--bp-violet)" }}>{repo?.ai_commits ?? "—"}</span>
      </span>
      <span>
        loaded:{" "}
        <span style={{ color: "var(--bp-text)" }}>{commits?.length ?? "—"}</span>
      </span>
      <span style={{ marginLeft: "auto", fontFamily: "monospace" }}>
        {branch ? <>branch: <span style={{ color: "var(--bp-blue-hi)" }}>{branch}</span> · </> : ""}
        mode: <span style={{ color: "var(--bp-blue-hi)" }}>{mode}</span>
        {selected && mode === "explore"
          ? ` ▸ ${selected.short_oid.slice(0, 12)}`
          : ""}
      </span>
    </div>
  );
}

function shortModel(m: string): string {
  return m
    .replace(/^claude-/, "")
    .replace(/-\d{8}$/, "")
    .replace(/-\d+-\d+$/, (s) => s);
}
