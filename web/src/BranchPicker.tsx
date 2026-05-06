import { useEffect, useState } from "react";
import {
  Button,
  Menu,
  MenuDivider,
  MenuItem,
  Popover,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import { api, type BranchInfo } from "./api";

// Branch picker — clickable tag in the header that opens a Blueprint Popover
// containing local + remote branches. Selecting a branch changes which ref
// the commit list walks from (via /api/commits?branch=X). The HEAD branch is
// the default selection.

export function BranchPicker({
  current,
  onChange,
  githubBranchUrl,
}: {
  current: string | null;
  onChange: (name: string) => void;
  githubBranchUrl: ((branch: string) => string | null) | null;
}) {
  const [branches, setBranches] = useState<BranchInfo[] | null>(null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open || branches !== null) return;
    api
      .branches()
      .then(setBranches)
      .catch(() => setBranches([]));
  }, [open, branches]);

  const local = branches?.filter((b) => !b.is_remote) ?? [];
  const remote = branches?.filter((b) => b.is_remote) ?? [];
  const ghUrl = current && githubBranchUrl ? githubBranchUrl(current) : null;

  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
      <Popover
        isOpen={open}
        onInteraction={(state) => setOpen(state)}
        placement="bottom-start"
        minimal
        content={
          <Menu className="wb-branch-menu">
            {branches === null ? (
              <MenuItem
                disabled
                text={
                  <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <Spinner size={12} /> Loading…
                  </span>
                }
              />
            ) : (
              <>
                {local.length > 0 ? (
                  <>
                    <MenuDivider title="Local" />
                    {local.map((b) => (
                      <MenuItem
                        key={b.name}
                        text={
                          <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                            {b.name}
                            {b.has_context_branch ? (
                              <span
                                className="wb-branch-ctx-dot"
                                title={
                                  b.context?.purpose
                                    ? `Context: ${b.context.purpose}`
                                    : "Has linked context branch"
                                }
                              />
                            ) : null}
                          </span>
                        }
                        icon={b.is_head ? "tick" : "git-branch"}
                        active={b.name === current}
                        onClick={() => {
                          onChange(b.name);
                          setOpen(false);
                        }}
                        labelElement={
                          b.ahead != null && (b.ahead > 0 || (b.behind ?? 0) > 0) ? (
                            <span
                              style={{
                                color: "var(--bp-text-dim)",
                                fontFamily: "monospace",
                                fontSize: 11,
                              }}
                            >
                              {b.ahead > 0 ? `↑${b.ahead}` : ""}
                              {(b.behind ?? 0) > 0 ? `↓${b.behind}` : ""}
                            </span>
                          ) : b.upstream ? (
                            <span
                              style={{
                                color: "var(--bp-text-dim)",
                                fontFamily: "monospace",
                                fontSize: 11,
                              }}
                            >
                              ↑ {b.upstream}
                            </span>
                          ) : undefined
                        }
                      />
                    ))}
                  </>
                ) : null}
                {remote.length > 0 ? (
                  <>
                    <MenuDivider title="Remote" />
                    {remote.map((b) => (
                      <MenuItem
                        key={b.name}
                        text={b.name}
                        icon="cloud"
                        active={b.name === current}
                        onClick={() => {
                          onChange(b.name);
                          setOpen(false);
                        }}
                      />
                    ))}
                  </>
                ) : null}
              </>
            )}
          </Menu>
        }
      >
        <Button
          minimal
          small
          rightIcon="caret-down"
          className="wb-branch-button"
          title={`Switch branch (current: ${current ?? "—"})`}
        >
          <Tag minimal className="wb-branch-tag">
            {current ?? "—"}
          </Tag>
        </Button>
      </Popover>
      {ghUrl ? (
        <a
          href={ghUrl}
          target="_blank"
          rel="noreferrer noopener"
          className="bp5-button bp5-minimal bp5-small"
          title="View branch on GitHub"
          style={{ minHeight: 24, padding: "0 6px" }}
        >
          <span className="bp5-icon bp5-icon-share" aria-hidden />
        </a>
      ) : null}
    </span>
  );
}
