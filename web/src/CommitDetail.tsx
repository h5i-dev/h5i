import { Tag } from "@blueprintjs/core";
import { githubCommitUrl, type Commit, type Repo } from "./api";

export function CommitDetail({
  commit,
  repo,
}: {
  commit: Commit;
  repo: Repo | null;
}) {
  const ghUrl = githubCommitUrl(repo?.github_url, commit.git_oid);
  return (
    <>
      <div className="wb-detail-section">
        <div className="wb-detail-msg">{commit.message.split("\n")[0]}</div>
        <div className="wb-detail-byline">
          <span style={{ color: "var(--bp-blue-hi)" }}>{commit.author}</span>{" "}
          · {formatTime(commit.timestamp)}
          {ghUrl ? (
            <>
              {" · "}
              <a href={ghUrl} target="_blank" rel="noreferrer noopener">
                GitHub ↗
              </a>
            </>
          ) : null}
        </div>
      </div>

      <div className="wb-detail-section">
        <div className="wb-detail-title">Identity</div>
        <div className="wb-detail-grid">
          <div className="wb-detail-key">SHA</div>
          <div className="wb-detail-val mono">
            {ghUrl ? (
              <a href={ghUrl} target="_blank" rel="noreferrer noopener">
                {commit.git_oid}
              </a>
            ) : (
              commit.git_oid
            )}
          </div>
          <div className="wb-detail-key">Short</div>
          <div className="wb-detail-val mono">{commit.short_oid}</div>
        </div>
      </div>

      {commit.ai_model || commit.ai_agent || commit.ai_prompt ? (
        <div className="wb-detail-section">
          <div className="wb-detail-title">AI provenance</div>
          <div className="wb-detail-grid">
            {commit.ai_model ? (
              <>
                <div className="wb-detail-key">Model</div>
                <div className="wb-detail-val">
                  <Tag intent="primary" minimal>
                    {commit.ai_model}
                  </Tag>
                </div>
              </>
            ) : null}
            {commit.ai_agent ? (
              <>
                <div className="wb-detail-key">Agent</div>
                <div className="wb-detail-val">
                  <Tag minimal style={{ background: "var(--bp-elev)" }}>
                    {commit.ai_agent}
                  </Tag>
                </div>
              </>
            ) : null}
            {commit.ai_tokens ? (
              <>
                <div className="wb-detail-key">Tokens</div>
                <div className="wb-detail-val mono">
                  {commit.ai_tokens.toLocaleString()}
                </div>
              </>
            ) : null}
            {commit.ai_prompt ? (
              <>
                <div className="wb-detail-key">Prompt</div>
                <div className="wb-detail-val wb-detail-prompt">
                  {commit.ai_prompt}
                </div>
              </>
            ) : null}
          </div>
        </div>
      ) : null}

      {commit.test_total != null ? (
        <div className="wb-detail-section">
          <div className="wb-detail-title">Tests</div>
          <div className="wb-detail-grid">
            <div className="wb-detail-key">Result</div>
            <div className="wb-detail-val">
              <Tag intent={commit.test_is_passing ? "success" : "danger"} minimal>
                {commit.test_is_passing ? "PASSING" : "FAILING"}
              </Tag>
            </div>
            <div className="wb-detail-key">Counts</div>
            <div className="wb-detail-val mono">
              {commit.test_passed ?? 0} pass · {commit.test_failed ?? 0} fail
              {commit.test_skipped ? ` · ${commit.test_skipped} skip` : ""}
            </div>
            {commit.test_coverage != null ? (
              <>
                <div className="wb-detail-key">Coverage</div>
                <div className="wb-detail-val mono">
                  {(commit.test_coverage * 100).toFixed(1)}%
                </div>
              </>
            ) : null}
            {commit.test_duration_secs != null ? (
              <>
                <div className="wb-detail-key">Duration</div>
                <div className="wb-detail-val mono">
                  {commit.test_duration_secs.toFixed(2)}s
                </div>
              </>
            ) : null}
          </div>
        </div>
      ) : null}

      {commit.ast_file_count != null && commit.ast_file_count > 0 ? (
        <div className="wb-detail-section">
          <div className="wb-detail-title">Structure</div>
          <div className="wb-detail-grid">
            <div className="wb-detail-key">AST snapshots</div>
            <div className="wb-detail-val mono">{commit.ast_file_count} files</div>
            {commit.has_crdt ? (
              <>
                <div className="wb-detail-key">CRDT</div>
                <div className="wb-detail-val">
                  <Tag intent="primary" minimal>
                    Y-CRDT session
                  </Tag>
                </div>
              </>
            ) : null}
          </div>
        </div>
      ) : null}
    </>
  );
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString();
}
