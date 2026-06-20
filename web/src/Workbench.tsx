import { useCallback, useEffect, useMemo, useState } from "react";
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
import { ContextStrip } from "./ContextStrip";
import { BranchPicker } from "./BranchPicker";

type Mode = "replay" | "cockpit" | "radio" | "explore" | "memory" | "context" | "sandbox";
type RightTab = "refs" | "sessions" | "integrity" | "context";

export function Workbench() {
  const [repo, setRepo] = useState<Repo | null>(null);
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedOid, setSelectedOid] = useState<string | null>(null);
  // Default landing is Replay — h5i's centerpiece: replay the agent run behind
  // the diff. Users jump to other modes via the header nav.
  const [mode, setMode] = useState<Mode>("replay");
  const [rightTab, setRightTab] = useState<RightTab>("refs");
  // When the cockpit asks to replay a specific commit, focus the replay there.
  const [replayFocusOid, setReplayFocusOid] = useState<string | null>(null);
  // null = follow HEAD (server default); a string = explicit branch override.
  const [activeBranch, setActiveBranch] = useState<string | null>(null);

  const loadRepo = useCallback(() => {
    api.repo().then(setRepo).catch((e) => setError(String(e)));
  }, []);

  const loadCommits = useCallback(
    (branch: string | null) => {
      setError(null);
      setCommits(null);
      api
        .commits({ limit: 200, branch })
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

  useEffect(() => {
    loadRepo();
  }, [loadRepo]);
  useEffect(() => {
    loadCommits(activeBranch);
  }, [activeBranch, loadCommits]);

  const refresh = () => {
    loadRepo();
    loadCommits(activeBranch);
  };

  const selected = useMemo(
    () => commits?.find((c) => c.git_oid === selectedOid) ?? null,
    [commits, selectedOid],
  );

  const jumpToCommit = (oid: string) => {
    setMode("explore");
    setSelectedOid(oid);
  };

  const branchInUI = activeBranch ?? repo?.branch ?? null;
  const ghBranchUrl = repo?.github_url
    ? (b: string) => githubBranchUrl(repo.github_url, b)
    : null;

  return (
    <div className="wb-shell">
      <header className="wb-header">
        <div className="wb-header-left">
          <span className="wb-brand">h5i</span>
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
              icon="play"
              text="Replay"
              active={mode === "replay"}
              onClick={() => setMode("replay")}
            />
            <Button
              icon="endorsed"
              text="Cockpit"
              active={mode === "cockpit"}
              onClick={() => setMode("cockpit")}
            />
            <Button
              icon="feed"
              text="Radio"
              active={mode === "radio"}
              onClick={() => setMode("radio")}
            />
            <Button
              icon="shield"
              text="Sandbox"
              active={mode === "sandbox"}
              onClick={() => setMode("sandbox")}
            />
            <Button
              icon="lightbulb"
              text="Context"
              active={mode === "context"}
              onClick={() => setMode("context")}
            />
            <Button
              icon="search-around"
              text="Explore"
              active={mode === "explore"}
              onClick={() => setMode("explore")}
            />
            <Button
              icon="database"
              text="Memory"
              active={mode === "memory"}
              onClick={() => setMode("memory")}
            />
          </ButtonGroup>
        </nav>

        <div className="wb-header-right">
          <QuickSearch
            commits={commits}
            onSelectCommit={jumpToCommit}
            onSelectBranch={(name) => {
              setActiveBranch(name);
              setMode("explore");
            }}
            onOpenContext={() => setMode("context")}
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
          <Button minimal small icon="refresh" onClick={refresh} title="Refresh" />
        </div>
      </header>

      <ContextStrip
        repoBranch={branchInUI}
        onOpen={() => setMode("context")}
      />

      {mode === "replay" ? (
        <ReplayView focusOid={replayFocusOid} />
      ) : mode === "cockpit" ? (
        <CockpitView
          onOpenReplay={(oid) => {
            setReplayFocusOid(oid);
            setMode("replay");
          }}
        />
      ) : mode === "radio" ? (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <RadioView branch={branchInUI} />
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
        <SandboxView />
      ) : (
        <div className="wb-body wb-body-single">
          <div className="wb-pane">
            <div className="wb-pane-header">Context workspace</div>
            <div className="wb-pane-body wb-context-body">
              <ContextView />
            </div>
          </div>
        </div>
      )}

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
  | { kind: "command"; id: string; title: string; detail: string; run: () => void };

function QuickSearch({
  commits,
  onSelectCommit,
  onSelectBranch,
  onOpenContext,
}: {
  commits: Commit[] | null;
  onSelectCommit: (oid: string) => void;
  onSelectBranch: (name: string) => void;
  onOpenContext: () => void;
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

    return [...commitHits, ...branchHits, ...commandHits.filter((h) => h.title.toLowerCase().includes(q))];
  }, [branches, commits, onOpenContext, query]);

  const choose = (hit: SearchHit) => {
    if (hit.kind === "commit") onSelectCommit(hit.oid);
    if (hit.kind === "branch") onSelectBranch(hit.name);
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
