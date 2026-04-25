//! MCP (Model Context Protocol) server for h5i.
//!
//! Implements the 2024-11-05 MCP specification over a newline-delimited
//! JSON-RPC 2.0 stdio transport.  Claude Code (or any MCP client) can point
//! at `h5i mcp` to gain native tool access to the h5i semantic layer.
//!
//! ## Tools exposed
//!
//! | Tool | h5i equivalent |
//! |------|----------------|
//! | `h5i_commit` | `h5i commit` |
//! | `h5i_rewind` | `h5i rewind` |
//! | `h5i_log` | `h5i log` |
//! | `h5i_blame` | `h5i blame` |
//! | `h5i_notes_show` | `h5i notes show` |
//! | `h5i_notes_uncertainty` | `h5i notes uncertainty` |
//! | `h5i_notes_coverage` | `h5i notes coverage` |
//! | `h5i_notes_review` | `h5i notes review` |
//! | `h5i_notes_churn` | `h5i notes churn` |
//! | `h5i_context_init` | `h5i context init` |
//! | `h5i_context_trace` | `h5i context trace` |
//! | `h5i_context_commit` | `h5i context commit` |
//! | `h5i_context_branch` | `h5i context branch` |
//! | `h5i_context_checkout` | `h5i context checkout` |
//! | `h5i_context_merge` | `h5i context merge` |
//! | `h5i_context_show` | `h5i context show` |
//! | `h5i_context_status` | `h5i context status` |
//! | `h5i_claims_add` | `h5i claims add` |
//! | `h5i_claims_list` | `h5i claims list` |
//! | `h5i_claims_prune` | `h5i claims prune` |
//! | `h5i_summary_set` | `h5i summary set` |
//! | `h5i_summary_get` | `h5i summary show` |
//! | `h5i_summary_list` | `h5i summary list` |
//! | `h5i_summary_prune` | `h5i summary prune` |
//!
//! ## Resources exposed
//!
//! | URI | Content |
//! |-----|---------|
//! | `h5i://context/current` | Live `GccContext` JSON (replaces `h5i context prompt`) |
//! | `h5i://log/recent` | 10 most recent commits with AI provenance |

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::blame::BlameMode;
use crate::claims;
use crate::ctx::{self, ContextOpts};
use crate::repository::H5iRepository;
use crate::session_log;
use crate::summaries;

// ── JSON-RPC 2.0 types ────────────────────────────────────────────────────────

/// An incoming JSON-RPC 2.0 request or notification.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// `None` for notifications (no response expected).
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// An outgoing JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// Successful response.
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ── MCP protocol version ──────────────────────────────────────────────────────

const PROTOCOL_VERSION: &str = "2024-11-05";

// ── Tool catalogue ────────────────────────────────────────────────────────────

/// Return the full list of MCP tool definitions.
pub fn tool_definitions() -> Value {
    json!([
        // ── log ──────────────────────────────────────────────────────────────
        {
            "name": "h5i_log",
            "description": "Return recent commits enriched with AI provenance metadata \
                (model, agent, prompt, token count) and test metrics. \
                Use this before editing a file to understand what AI work has already \
                been done and why.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of commits to return (default 20)."
                    }
                }
            }
        },
        // ── blame ─────────────────────────────────────────────────────────────
        {
            "name": "h5i_blame",
            "description": "Show per-line authorship for a file, enriched with AI metadata \
                (model, prompt) and test pass/fail status at the time of each commit. \
                Use this to understand which lines were written by AI vs humans, and \
                which prompts produced them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Relative path to the file to blame."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["line", "ast"],
                        "description": "Blame granularity: 'line' (default) or 'ast' for \
                            semantic node-level attribution."
                    }
                },
                "required": ["file"]
            }
        },
        // ── notes ─────────────────────────────────────────────────────────────
        {
            "name": "h5i_notes_show",
            "description": "Show the full session analysis for a commit: exploration \
                footprint (files read vs edited), causal chain (trigger → decisions → \
                edits), omissions, and file coverage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commit": {
                        "type": "string",
                        "description": "Commit OID or prefix to inspect (default: HEAD)."
                    }
                }
            }
        },
        {
            "name": "h5i_notes_uncertainty",
            "description": "List moments where the AI expressed uncertainty in its thinking \
                blocks during a session (phrases like 'not sure', 'might break', \
                'need to verify'). Each entry includes a confidence score, context \
                snippet, and file being edited at the time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commit": {
                        "type": "string",
                        "description": "Commit OID or prefix (default: HEAD)."
                    },
                    "file": {
                        "type": "string",
                        "description": "Filter to uncertainties expressed while editing \
                            a specific file path."
                    }
                }
            }
        },
        {
            "name": "h5i_notes_coverage",
            "description": "Show per-file attention coverage: which files were edited \
                without being read first (blind edits). High blind-edit counts are a \
                signal that the AI modified a file from memory rather than reading its \
                current state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commit": {
                        "type": "string",
                        "description": "Commit OID or prefix (default: HEAD)."
                    },
                    "max_ratio": {
                        "type": "number",
                        "description": "Only return files with read-before-edit ratio \
                            at or below this value (0.0–1.0). Omit to return all files."
                    }
                }
            }
        },
        {
            "name": "h5i_notes_review",
            "description": "Return commits ranked by review worthiness. Scoring is \
                deterministic and based on signals such as: large diff, high uncertainty \
                expressions, blind edits, AI-only authorship, no test coverage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Number of recent commits to scan (default 50)."
                    },
                    "min_score": {
                        "type": "number",
                        "description": "Minimum review score threshold, 0.0–1.0 \
                            (default 0.4). Lower values return more commits."
                    }
                }
            }
        },
        {
            "name": "h5i_notes_churn",
            "description": "Return aggregate file churn statistics across all analyzed \
                sessions: edit count, read count, and churn score per file. High churn \
                scores indicate files that are frequently edited relative to how often \
                they are read — a fragility signal.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        // ── commit ────────────────────────────────────────────────────────────
        {
            "name": "h5i_commit",
            "description": "Create a git commit that records AI provenance (model, agent, prompt). \
                IMPORTANT: files must be staged with `git add` before calling this — \
                it will return an error with instructions if nothing is staged. \
                Always prefer this over raw `git commit` so provenance is captured.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Commit message."
                    },
                    "model": {
                        "type": "string",
                        "description": "AI model name (e.g. 'claude-sonnet-4-6')."
                    },
                    "agent": {
                        "type": "string",
                        "description": "Agent identifier (e.g. 'claude-code')."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The user prompt that triggered this change."
                    }
                },
                "required": ["message"]
            }
        },
        // ── rewind ────────────────────────────────────────────────────────────
        {
            "name": "h5i_rewind",
            "description": "Restore the working tree to the exact file state of any past commit \
                without moving HEAD. Use this to recover from a bad agent run: files are \
                overwritten to match the target commit, HEAD stays put, and `git diff HEAD` \
                shows the full picture before you decide whether to commit. \
                Current dirty state is saved to refs/h5i/shadow/<timestamp> first so \
                recovery is always possible. Accepts full or short SHAs and rev expressions \
                like HEAD~3.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sha": {
                        "type": "string",
                        "description": "Git commit SHA or rev expression to restore (e.g. 'abc1234', 'HEAD~2')."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, return the list of files that would change without touching the working tree."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "If true, skip saving dirty state to a shadow ref before rewinding."
                    }
                },
                "required": ["sha"]
            }
        },
        // ── notes/analyze ─────────────────────────────────────────────────────
        {
            "name": "h5i_notes_analyze",
            "description": "Parse the current Claude Code session log and store enriched \
                metadata (exploration footprint, causal chain, uncertainty moments, \
                file churn) linked to HEAD. Call this once at the end of every session, \
                after the final h5i_commit.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        // ── context ───────────────────────────────────────────────────────────
        {
            "name": "h5i_context_init",
            "description": "Initialize the h5i reasoning workspace for this project. \
                Call once at the start of a major task to set the project goal. \
                Safe to call again to update the goal.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "High-level goal for this reasoning session \
                            (e.g. 'refactor HTTP client to support retries')."
                    }
                }
            }
        },
        {
            "name": "h5i_context_trace",
            "description": "Append one OTA (Observe/Think/Act/Note) step to the reasoning trace. \
                Emit one entry per distinct insight — not one per phase: \
                one OBSERVE per file read (say what matters, not just 'read the file'); \
                one THINK per decision, always naming what you rejected and why; \
                one ACT per file written or edited; \
                NOTE is REQUIRED whenever you leave something incomplete, find a risk, \
                or choose an approach with a known limitation. \
                Auto-initializes the workspace if it does not yet exist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["OBSERVE", "THINK", "ACT", "NOTE"],
                        "description": "Type of trace entry."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content of the trace step."
                    }
                },
                "required": ["kind", "content"]
            }
        },
        {
            "name": "h5i_context_commit",
            "description": "Checkpoint the current reasoning progress with a summary \
                milestone. Analogous to `git commit` but for the reasoning workspace. \
                Auto-initializes the workspace if needed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "One-line summary of progress achieved at this \
                            checkpoint."
                    },
                    "detail": {
                        "type": "string",
                        "description": "Optional detailed description of decisions made \
                            and work completed."
                    }
                },
                "required": ["summary"]
            }
        },
        {
            "name": "h5i_context_branch",
            "description": "Create a new reasoning branch to explore an alternative \
                approach without losing the current thread. Analogous to `git branch`. \
                Auto-initializes the workspace if needed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Branch name (e.g. 'experiment/sync-retry')."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Why this branch is being explored."
                    }
                },
                "required": ["name"]
            }
        },
        {
            "name": "h5i_context_checkout",
            "description": "Switch to an existing reasoning branch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the branch to switch to."
                    }
                },
                "required": ["name"]
            }
        },
        {
            "name": "h5i_context_merge",
            "description": "Merge a completed reasoning branch back into the current \
                branch, synthesizing findings from both threads.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "branch": {
                        "type": "string",
                        "description": "Name of the branch to merge from."
                    }
                },
                "required": ["branch"]
            }
        },
        {
            "name": "h5i_context_show",
            "description": "Return the current context workspace state as structured JSON: \
                project goal, milestones, active branches, recent checkpoint summaries, \
                and (optionally) recent trace lines.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "branch": {
                        "type": "string",
                        "description": "Branch to inspect (default: current active branch)."
                    },
                    "window": {
                        "type": "integer",
                        "description": "Number of recent checkpoints to include (default 3)."
                    },
                    "trace": {
                        "type": "boolean",
                        "description": "Include recent OTA trace lines (default false)."
                    }
                }
            }
        },
        {
            "name": "h5i_context_status",
            "description": "Return a compact summary of the reasoning workspace: \
                initialization state, active branch, all branch names, and branch count.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "h5i_context_knowledge",
            "description": "Distill all THINK entries from every context branch into a \
                project knowledge base. Returns a deduplicated list of design decisions \
                recorded across all reasoning branches. Use this at session start to \
                quickly re-absorb the project's accumulated design rationale.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        // ── context versioning ─────────────────────────────────────────────────
        {
            "name": "h5i_context_restore",
            "description": "Restore the context workspace to the state captured when a \
                specific git commit was made. Every `h5i commit` snapshots context \
                automatically; this tool replays that snapshot non-destructively by \
                appending a new commit to refs/h5i/context, so the full history is \
                preserved. Use this at session start to continue exactly where a \
                previous session left off instead of re-deriving context from scratch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sha": {
                        "type": "string",
                        "description": "Git commit SHA to restore context from. \
                            Prefix form accepted (e.g. 'a3f8c12')."
                    }
                },
                "required": ["sha"]
            }
        },
        {
            "name": "h5i_context_diff",
            "description": "Show how the context workspace evolved between two git commits. \
                Returns new reasoning milestones, new OTA trace steps, and any change to \
                the project goal. Both commits must have context snapshots (created \
                automatically by `h5i commit`). Use this to understand what the AI \
                learned or decided between two points in history.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": {
                        "type": "string",
                        "description": "Earlier git commit SHA (prefix accepted)."
                    },
                    "to": {
                        "type": "string",
                        "description": "Later git commit SHA (prefix accepted)."
                    }
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "h5i_context_relevant",
            "description": "Return all context workspace entries that mention a specific \
                file: milestone contributions, OTA trace lines (with one line of surrounding \
                context), and cross-branch mentions from other reasoning branches. \
                Call this BEFORE editing a file to recover accumulated reasoning — past \
                decisions, uncertainties, and actions — without re-reading the full trace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "File path to look up (e.g. 'src/repository.rs'). \
                            Matched against both the full path and the filename."
                    }
                },
                "required": ["file"]
            }
        },
        {
            "name": "h5i_context_scan",
            "description": "Scan the current branch's OTA trace for prompt-injection \
                patterns and return a 0.0–1.0 risk score. Use after sessions that \
                processed external data (files, web pages, tool output) to detect \
                whether any injected instructions contaminated the reasoning trace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "branch": {
                        "type": "string",
                        "description": "Branch to scan (default: current active branch)."
                    }
                }
            }
        },
        {
            "name": "h5i_context_pack",
            "description": "Compact old context history by squashing refs/h5i/context \
                commits that predate the earliest linked code-commit snapshot. \
                Appends a marker to main.md. Run `git gc` afterwards to reclaim \
                disk space. Returns the number of commits squashed.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        // ── context search ─────────────────────────────────────────────────────
        {
            "name": "h5i_context_search",
            "description": "Search context traces and session footprints for files relevant \
                to a natural-language query. Combines BM25-style scoring over \
                OBSERVE/THINK/ACT trace entries with git co-change analysis. \
                Use this INSTEAD of exploratory Grep/Read chains to find which files \
                are relevant to a task — it returns ranked candidates with evidence \
                snippets and co-change partners, dramatically reducing token consumption \
                during codebase exploration.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language search query (e.g. 'auth token expiry' \
                            or 'retry logic timeout')."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of file results to return. Default: 10."
                    },
                    "history": {
                        "type": "integer",
                        "description": "Number of git commits to walk for co-change analysis. \
                            Default: 200."
                    }
                },
                "required": ["query"]
            }
        },
        // ── claims ─────────────────────────────────────────────────────────────
        {
            "name": "h5i_claims_add",
            "description": "Record a content-addressed claim — a short fact about the \
                codebase that a future session can treat as pre-verified. The claim \
                is pinned via a Merkle hash over the current blob OIDs of the \
                evidence paths at HEAD, so it auto-invalidates the instant any \
                evidence file changes. Use this AFTER pinning down a non-obvious \
                fact that would otherwise be re-derived: where a helper lives, which \
                module owns a concern, a subtle invariant, or where NOT to look. \
                Skip it for trivia a quick grep would answer. All evidence paths \
                must be tracked in HEAD. Keep evidence paths tight — over-listing \
                makes the claim go stale on unrelated edits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The claim itself (e.g. 'HTTP helpers live only in \
                            src/api/client.py'). Keep it short, specific, and reusable."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File paths tracked in HEAD that are the evidence for \
                            this claim. Any edit to any of these files will invalidate the \
                            claim. List ONLY the files that genuinely back the statement.",
                        "minItems": 1
                    },
                    "author": {
                        "type": "string",
                        "description": "Optional author tag. Defaults to $H5I_AGENT_ID, \
                            then 'human'."
                    }
                },
                "required": ["text", "paths"]
            }
        },
        {
            "name": "h5i_claims_list",
            "description": "List all recorded claims with their live/stale status. A \
                claim is 'live' when its evidence blobs match the recorded Merkle \
                hash at HEAD, and 'stale' when any evidence file has changed. \
                Use this to see what facts are already pinned before redoing \
                exploratory work — the SessionStart prelude already surfaces live \
                claims automatically, so this is mainly for verification or \
                debugging.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "h5i_claims_prune",
            "description": "Delete every claim whose evidence blobs no longer match \
                HEAD (status = stale). Live claims are left untouched. Returns the \
                number of claims removed. Safe to run at the end of a session that \
                made significant edits — it clears out guidance that the code has \
                outgrown, so the next session is not misled by stale pins.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        // ── summaries ──────────────────────────────────────────────────────────
        {
            "name": "h5i_summary_set",
            "description": "Pin a short summary (≈100–300 tokens, markdown) to the \
                CURRENT HEAD blob of `path`. Use this to record what's in a file — \
                exports, key types, role, structure — so future sessions can fetch \
                this orientation instead of re-reading the file. Each git blob is \
                immutable, so the summary stays correct for that exact content \
                forever; when the file is edited, HEAD points at a new blob with \
                no summary yet (the old one is preserved but inaccessible by path \
                lookup until someone writes a new one). Distinct from claims: \
                claims pin cross-file FACTS; summaries describe ONE file's CONTENT.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path tracked in HEAD."
                    },
                    "text": {
                        "type": "string",
                        "description": "The summary text. Keep it dense — exports, role, \
                            invariants. ≤300 tokens is the sweet spot."
                    },
                    "author": {
                        "type": "string",
                        "description": "Optional author tag. Defaults to $H5I_AGENT_ID, \
                            then 'human'."
                    }
                },
                "required": ["path", "text"]
            }
        },
        {
            "name": "h5i_summary_get",
            "description": "Fetch the summary for `path`'s CURRENT HEAD blob. \
                Returns the summary text + metadata when one exists for the exact \
                blob HEAD points at, otherwise indicates none is available. \
                PREFER THIS over `Read` when you only need orientation (which \
                file does what, what's exported, where to look for X). Reading \
                the full file is still correct for line-level edits — but for \
                navigation a summary is dramatically cheaper.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path tracked in HEAD."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "h5i_summary_list",
            "description": "List every HEAD-tracked file with a flag for whether a \
                summary exists for its current blob. Use this at session start to \
                see which files you can orient on cheaply via h5i_summary_get and \
                which still require a full Read. Returns counts (with-summary, \
                without-summary, total) plus per-file rows.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "h5i_summary_prune",
            "description": "Delete summaries whose blob OID is no longer reachable \
                from any HEAD-tracked path (i.e. the blob has been replaced \
                everywhere). Live summaries are left untouched. Returns the \
                number of summaries removed. Safe periodic cleanup; not required \
                for correctness because each summary is content-addressed and \
                cannot become 'wrong'.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ])
}

// ── Resource catalogue ────────────────────────────────────────────────────────

/// Return the full list of MCP resource definitions.
pub fn resource_definitions() -> Value {
    json!([
        {
            "uri": "h5i://context/current",
            "name": "Current Reasoning Context",
            "description": "The live h5i context workspace state: project goal, \
                milestones, current branch, recent checkpoint summaries, and OTA trace. \
                Inject this resource at session start for full context continuity — \
                it replaces the need to call h5i_context_show manually.",
            "mimeType": "application/json"
        },
        {
            "uri": "h5i://context/snapshots",
            "name": "Context Snapshots",
            "description": "List of git commits that have a linked context snapshot, \
                with their branch, goal summary, and timestamp. Use this to discover \
                which commits can be passed to h5i_context_restore or h5i_context_diff.",
            "mimeType": "application/json"
        },
        {
            "uri": "h5i://log/recent",
            "name": "Recent Commits",
            "description": "The 10 most recent commits enriched with AI provenance \
                metadata (model, agent, prompt, token count) and test metrics.",
            "mimeType": "application/json"
        }
    ])
}

// ── Tool helpers ──────────────────────────────────────────────────────────────

/// Wrap a plain string as an MCP text content block.
fn text_content(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }]
    })
}

/// Wrap a serialisable value as an MCP content block with structured JSON.
/// Returns both a human-readable text representation and the raw JSON so
/// callers can parse it without double-decoding a string.
fn json_content(v: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }]
    })
}

/// Resolve a caller-supplied commit OID string (or None → HEAD) to a full OID
/// string using the repository.
fn resolve_oid(repo: &H5iRepository, commit: Option<&str>) -> Result<String> {
    match commit {
        Some(oid) => Ok(oid.to_string()),
        None => {
            let head = repo.git().head()?.peel_to_commit()?;
            Ok(head.id().to_string())
        }
    }
}

// ── Tool handlers ─────────────────────────────────────────────────────────────

fn tool_log(params: &Value, workdir: &Path) -> Result<Value> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20) as usize;
    let repo = H5iRepository::open(workdir)?;
    let records = repo.get_log(limit)?;
    Ok(json_content(serde_json::to_value(&records)?))
}

fn tool_blame(params: &Value, workdir: &Path) -> Result<Value> {
    let file = params
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: file"))?;
    let mode = match params.get("mode").and_then(Value::as_str).unwrap_or("line") {
        "ast" => BlameMode::Ast,
        _ => BlameMode::Line,
    };
    let repo = H5iRepository::open(workdir)?;
    let results = repo.blame(Path::new(file), mode)?;
    Ok(json_content(serde_json::to_value(&results)?))
}

fn tool_notes_show(params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let oid = resolve_oid(&repo, params.get("commit").and_then(Value::as_str))?;
    match session_log::load_analysis(repo.h5i_path(), &oid)? {
        Some(analysis) => Ok(json_content(serde_json::to_value(&analysis)?)),
        None => Ok(text_content(format!("No session notes found for {:.8}", oid))),
    }
}

fn tool_notes_uncertainty(params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let oid = resolve_oid(&repo, params.get("commit").and_then(Value::as_str))?;
    let file_filter = params.get("file").and_then(Value::as_str);
    match session_log::load_analysis(repo.h5i_path(), &oid)? {
        Some(analysis) => {
            let filtered: Vec<_> = analysis
                .uncertainty
                .iter()
                .filter(|u| {
                    file_filter
                        .map(|f| u.context_file.contains(f))
                        .unwrap_or(true)
                })
                .collect();
            Ok(json_content(serde_json::to_value(filtered)?))
        }
        None => Ok(text_content(format!("No session notes found for {:.8}", oid))),
    }
}

fn tool_notes_coverage(params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let oid = resolve_oid(&repo, params.get("commit").and_then(Value::as_str))?;
    let max_ratio = params.get("max_ratio").and_then(Value::as_f64);
    match session_log::load_analysis(repo.h5i_path(), &oid)? {
        Some(analysis) => {
            let filtered: Vec<_> = analysis
                .coverage
                .iter()
                .filter(|fc| {
                    max_ratio
                        .map(|max| fc.read_before_edit_ratio as f64 <= max)
                        .unwrap_or(true)
                })
                .collect();
            Ok(json_content(serde_json::to_value(filtered)?))
        }
        None => Ok(text_content(format!("No session notes found for {:.8}", oid))),
    }
}

fn tool_notes_review(params: &Value, workdir: &Path) -> Result<Value> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50) as usize;
    let min_score = params
        .get("min_score")
        .and_then(Value::as_f64)
        .unwrap_or(0.4) as f32;
    let repo = H5iRepository::open(workdir)?;
    let points = repo.suggest_review_points(limit, min_score)?;
    Ok(json_content(serde_json::to_value(&points)?))
}

fn tool_notes_churn(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let churn = session_log::aggregate_churn(repo.h5i_path());
    Ok(json_content(serde_json::to_value(churn)?))
}

fn tool_commit(params: &Value, workdir: &Path) -> Result<Value> {
    use crate::metadata::{AiMetadata, TestSource};

    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: message"))?;
    let model = params.get("model").and_then(Value::as_str);
    let agent = params.get("agent").and_then(Value::as_str);
    let prompt = params.get("prompt").and_then(Value::as_str);

    let repo = H5iRepository::open(workdir)?;
    let sig = repo.git().signature()?;

    // Refuse if nothing staged.
    let idx = repo.git().index()?;
    let head_empty = repo.git().head().is_err();
    let staged = if head_empty {
        !idx.is_empty()
    } else {
        let head_tree = repo.git().head()?.peel_to_tree()?;
        let diff = repo.git().diff_tree_to_index(Some(&head_tree), Some(&idx), None)?;
        diff.deltas().len() > 0
    };
    if !staged {
        return Ok(json_content(json!({
            "success": false,
            "error": "Nothing staged. Stage files with `git add <file>` first, then call h5i_commit again."
        })));
    }

    let ai_meta = if model.is_some() || agent.is_some() || prompt.is_some() {
        Some(AiMetadata {
            model_name: model.unwrap_or("unknown").to_string(),
            agent_id: agent.unwrap_or("unknown").to_string(),
            prompt: prompt.unwrap_or("").to_string(),
            usage: None,
        })
    } else {
        None
    };

    let oid = repo.commit(
        message, &sig, &sig, ai_meta, TestSource::None, None, vec![], vec![],
    )?;

    // Snapshot context if initialized.
    let _ = ctx::snapshot_for_commit(workdir, &oid.to_string());

    Ok(json_content(json!({
        "success": true,
        "sha": &oid.to_string()[..8],
        "message": message
    })))
}

fn tool_rewind(params: &Value, workdir: &Path) -> Result<Value> {
    let sha = params
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: sha"))?;
    let dry_run = params.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let force   = params.get("force").and_then(Value::as_bool).unwrap_or(false);

    let repo = H5iRepository::open(workdir)?;
    let (shadow_ref, changed) = repo.rewind(sha, force, dry_run)?;

    let files: Vec<serde_json::Value> = changed
        .iter()
        .map(|(p, k)| serde_json::json!({ "path": p, "change": k }))
        .collect();

    Ok(json_content(serde_json::json!({
        "dry_run": dry_run,
        "target_sha": sha,
        "shadow_ref": shadow_ref,
        "files_changed": files.len(),
        "files": files,
    })))
}

fn tool_notes_analyze(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let head_oid = repo.git().head()?.peel_to_commit()?.id().to_string();

    let session_path = session_log::find_latest_session(workdir)
        .ok_or_else(|| anyhow::anyhow!("No Claude session log found for this project"))?;

    let analysis = session_log::analyze_session(&session_path, None)?;
    let msg_count = analysis.message_count;
    session_log::save_analysis(repo.h5i_path(), &head_oid, &analysis)?;

    Ok(json_content(json!({
        "success": true,
        "commit": &head_oid[..8],
        "messages_analyzed": msg_count,
        "session_log": session_path.display().to_string()
    })))
}

fn tool_context_init(params: &Value, workdir: &Path) -> Result<Value> {
    let goal = params.get("goal").and_then(Value::as_str).unwrap_or("");
    ctx::init(workdir, goal)?;
    Ok(text_content("Context workspace initialized."))
}

fn tool_context_trace(params: &Value, workdir: &Path) -> Result<Value> {
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: kind"))?;
    let content = params
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: content"))?;
    // Auto-initialize so callers don't need a separate init step.
    if !ctx::is_initialized(workdir) {
        ctx::init(workdir, "")?;
    }
    ctx::append_log(workdir, kind, content, false)?;
    Ok(text_content(format!("[{}] {}", kind, content)))
}

fn tool_context_commit(params: &Value, workdir: &Path) -> Result<Value> {
    let summary = params
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: summary"))?;
    let detail = params
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or(summary);
    if !ctx::is_initialized(workdir) {
        ctx::init(workdir, "")?;
    }
    ctx::gcc_commit(workdir, summary, detail)?;
    Ok(text_content(format!("Checkpoint saved: {}", summary)))
}

fn tool_context_branch(params: &Value, workdir: &Path) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: name"))?;
    let purpose = params.get("purpose").and_then(Value::as_str).unwrap_or("");
    if !ctx::is_initialized(workdir) {
        ctx::init(workdir, "")?;
    }
    ctx::gcc_branch(workdir, name, purpose)?;
    Ok(text_content(format!("Branch '{}' created and checked out.", name)))
}

fn tool_context_checkout(params: &Value, workdir: &Path) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: name"))?;
    ctx::gcc_checkout(workdir, name)?;
    Ok(text_content(format!("Switched to branch '{}'.", name)))
}

fn tool_context_merge(params: &Value, workdir: &Path) -> Result<Value> {
    let branch = params
        .get("branch")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: branch"))?;
    let summary = ctx::gcc_merge(workdir, branch)?;
    Ok(text_content(format!("Merged '{}': {}", branch, summary)))
}

fn tool_context_show(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false, "message": "Context workspace not initialized. Call h5i_context_init first."})));
    }
    let opts = ContextOpts {
        branch: params
            .get("branch")
            .and_then(Value::as_str)
            .map(str::to_string),
        commit_hash: None,
        show_log: params
            .get("trace")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        log_offset: 0,
        metadata_segment: None,
        window: params
            .get("window")
            .and_then(Value::as_u64)
            .unwrap_or(3) as usize,
        depth: params
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(2) as u8,
    };
    let context = ctx::gcc_context(workdir, &opts)?;
    Ok(json_content(serde_json::to_value(&context)?))
}

fn tool_context_status(_params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false})));
    }
    let current = ctx::current_branch(workdir);
    let branches = ctx::list_branches(workdir);
    let branch_count = branches.len();
    Ok(json_content(json!({
        "initialized": true,
        "current_branch": current,
        "branches": branches,
        "branch_count": branch_count
    })))
}

fn tool_context_knowledge(_params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false, "thoughts": []})));
    }
    let thoughts = ctx::distill_knowledge(workdir)?;
    Ok(json_content(json!({ "thoughts": thoughts })))
}

fn tool_context_restore(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        ctx::init(workdir, "")?;
    }
    let sha = params
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: sha"))?;
    let summary = ctx::restore(workdir, sha)?;
    Ok(text_content(format!("Context restored: {summary}")))
}

fn tool_context_diff(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false})));
    }
    let from = params
        .get("from")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: from"))?;
    let to = params
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: to"))?;
    let diff = ctx::context_diff(workdir, from, to)?;
    Ok(json_content(json!({
        "from": diff.sha1,
        "to": diff.sha2,
        "goal_changed": diff.goal_changed,
        "from_goal": diff.from_goal,
        "to_goal": diff.to_goal,
        "added_milestones": diff.added_commits,
        "added_trace_lines": diff.added_trace_lines
    })))
}

fn tool_context_relevant(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false, "file": params.get("file"), "milestone_mentions": [], "trace_mentions": [], "cross_branch_mentions": []})));
    }
    let file = params
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: file"))?;
    let result = ctx::relevant(workdir, file)?;
    Ok(json_content(json!({
        "file": file,
        "milestone_mentions": result.commit_mentions,
        "trace_mentions": result.trace_mentions,
        "cross_branch_mentions": result.cross_branch_mentions
    })))
}

fn tool_context_scan(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false, "score": 0.0})));
    }
    let branch = params.get("branch").and_then(Value::as_str);
    let trace = ctx::read_trace(workdir, branch)?;
    let result = crate::injection::scan(&trace);
    Ok(json_content(serde_json::to_value(&result)?))
}

fn tool_context_search(params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"initialized": false, "results": []})));
    }
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: query"))?;
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let history = params.get("history").and_then(Value::as_u64).unwrap_or(200) as usize;

    let mut results = ctx::search(workdir, query, limit)?;

    // Enrich top results with co-change data from git
    if let Ok(repo) = crate::repository::H5iRepository::open(workdir) {
        for r in results.iter_mut().take(5) {
            if let Ok(cochanged) = repo.cochanged_files(&r.file, history, 5) {
                r.cochanged_with = cochanged.into_iter().map(|(f, _)| f).collect();
            }
        }
    }

    let out: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "file": r.file,
                "score": r.score,
                "signal": r.signal,
                "snippets": r.snippets,
                "cochanged_with": r.cochanged_with,
            })
        })
        .collect();
    Ok(json_content(json!({ "query": query, "results": out })))
}

fn tool_context_pack(_params: &Value, workdir: &Path) -> Result<Value> {
    if !ctx::is_initialized(workdir) {
        return Ok(json_content(json!({"squashed_commits": 0, "message": "Nothing to pack — workspace not initialized."})));
    }
    let squashed = ctx::pack(workdir)?;
    Ok(json_content(json!({
        "squashed_commits": squashed,
        "message": if squashed == 0 {
            "Nothing to pack — context history is already compact.".to_string()
        } else {
            format!("Packed {squashed} old context commits. Run `git gc` to reclaim disk space.")
        }
    })))
}

// ── Claims handlers ───────────────────────────────────────────────────────────

fn tool_claims_add(params: &Value, workdir: &Path) -> Result<Value> {
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: text"))?;
    let paths_value = params
        .get("paths")
        .ok_or_else(|| anyhow::anyhow!("missing required param: paths"))?;
    let paths: Vec<String> = paths_value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("paths must be an array of strings"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("paths must contain strings only"))
        })
        .collect::<Result<Vec<_>>>()?;
    if paths.is_empty() {
        anyhow::bail!("paths must contain at least one element");
    }
    let author = params
        .get("author")
        .and_then(Value::as_str)
        .map(str::to_string);

    let repo = H5iRepository::open(workdir)?;
    let claim = claims::add(&repo.h5i_root, repo.git(), text, paths, author)?;
    Ok(json_content(json!({
        "id": claim.id,
        "text": claim.text,
        "evidence_paths": claim.evidence_paths,
        "evidence_oid": claim.evidence_oid,
        "author": claim.author,
        "created_at": claim.created_at.to_rfc3339(),
    })))
}

fn tool_claims_list(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let entries = claims::list_with_status(&repo.h5i_root, repo.git())?;
    let rendered: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "id": e.claim.id,
                "text": e.claim.text,
                "status": match e.status {
                    claims::ClaimStatus::Live => "live",
                    claims::ClaimStatus::Stale => "stale",
                },
                "evidence_paths": e.claim.evidence_paths,
                "evidence_oid": e.claim.evidence_oid,
                "current_evidence_oid": e.current_evidence_oid,
                "author": e.claim.author,
                "created_at": e.claim.created_at.to_rfc3339(),
            })
        })
        .collect();
    let live = entries
        .iter()
        .filter(|e| e.status == claims::ClaimStatus::Live)
        .count();
    let stale = entries.len() - live;
    Ok(json_content(json!({
        "claims": rendered,
        "live": live,
        "stale": stale,
    })))
}

fn tool_claims_prune(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let removed = claims::prune_stale(&repo.h5i_root, repo.git())?;
    Ok(json_content(json!({
        "removed": removed,
        "message": if removed == 0 {
            "No stale claims — nothing to prune.".to_string()
        } else {
            format!("Pruned {removed} stale claim(s).")
        }
    })))
}

// ── Summary handlers ──────────────────────────────────────────────────────────

fn tool_summary_set(params: &Value, workdir: &Path) -> Result<Value> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: path"))?;
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: text"))?;
    let author = params
        .get("author")
        .and_then(Value::as_str)
        .map(str::to_string);
    let repo = H5iRepository::open(workdir)?;
    let s = summaries::set(&repo.h5i_root, repo.git(), path, text, author)?;
    Ok(json_content(json!({
        "blob_oid": s.blob_oid,
        "path": s.path,
        "text_chars": s.text.len(),
        "author": s.author,
        "created_at": s.created_at.to_rfc3339(),
    })))
}

fn tool_summary_get(params: &Value, workdir: &Path) -> Result<Value> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required param: path"))?;
    let repo = H5iRepository::open(workdir)?;
    let blob_oid = summaries::blob_oid_at_head(repo.git(), path)?;
    match summaries::get_by_blob(&repo.h5i_root, &blob_oid)? {
        Some(s) => Ok(json_content(json!({
            "found": true,
            "blob_oid": s.blob_oid,
            "path": s.path,
            "text": s.text,
            "author": s.author,
            "created_at": s.created_at.to_rfc3339(),
        }))),
        None => Ok(json_content(json!({
            "found": false,
            "blob_oid": blob_oid,
            "path": path,
            "message": "No summary for this path's current HEAD blob. \
                       Use Read for full content, then optionally h5i_summary_set.",
        }))),
    }
}

fn tool_summary_list(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let head = summaries::list_for_head(&repo.h5i_root, repo.git())?;
    let with: Vec<Value> = head
        .iter()
        .filter(|s| s.summary.is_some())
        .map(|s| json!({"path": s.path, "blob_oid": s.current_blob_oid}))
        .collect();
    let without: Vec<Value> = head
        .iter()
        .filter(|s| s.summary.is_none())
        .map(|s| json!({"path": s.path, "blob_oid": s.current_blob_oid}))
        .collect();
    Ok(json_content(json!({
        "with_summary": with,
        "without_summary": without,
        "with_count": with.len(),
        "without_count": without.len(),
        "total": head.len(),
    })))
}

fn tool_summary_prune(_params: &Value, workdir: &Path) -> Result<Value> {
    let repo = H5iRepository::open(workdir)?;
    let removed = summaries::prune_unreachable(&repo.h5i_root, repo.git())?;
    Ok(json_content(json!({
        "removed": removed,
        "message": if removed == 0 {
            "No unreachable summaries — nothing to prune.".to_string()
        } else {
            format!("Pruned {removed} orphaned summary blob(s).")
        }
    })))
}

// ── Tool call dispatch ────────────────────────────────────────────────────────

/// Dispatch a `tools/call` invocation to the appropriate handler.
///
/// Returns `Err` only for unknown tool names — individual tool errors are
/// wrapped in an `isError: true` MCP content response by the caller.
pub fn call_tool(name: &str, params: &Value, workdir: &Path) -> Result<Value> {
    match name {
        "h5i_commit" => tool_commit(params, workdir),
        "h5i_rewind" => tool_rewind(params, workdir),
        "h5i_notes_analyze" => tool_notes_analyze(params, workdir),
        "h5i_log" => tool_log(params, workdir),
        "h5i_blame" => tool_blame(params, workdir),
        "h5i_notes_show" => tool_notes_show(params, workdir),
        "h5i_notes_uncertainty" => tool_notes_uncertainty(params, workdir),
        "h5i_notes_coverage" => tool_notes_coverage(params, workdir),
        "h5i_notes_review" => tool_notes_review(params, workdir),
        "h5i_notes_churn" => tool_notes_churn(params, workdir),
        "h5i_context_init" => tool_context_init(params, workdir),
        "h5i_context_trace" => tool_context_trace(params, workdir),
        "h5i_context_commit" => tool_context_commit(params, workdir),
        "h5i_context_branch" => tool_context_branch(params, workdir),
        "h5i_context_checkout" => tool_context_checkout(params, workdir),
        "h5i_context_merge" => tool_context_merge(params, workdir),
        "h5i_context_show" => tool_context_show(params, workdir),
        "h5i_context_status" => tool_context_status(params, workdir),
        "h5i_context_knowledge" => tool_context_knowledge(params, workdir),
        "h5i_context_restore" => tool_context_restore(params, workdir),
        "h5i_context_diff" => tool_context_diff(params, workdir),
        "h5i_context_relevant" => tool_context_relevant(params, workdir),
        "h5i_context_scan" => tool_context_scan(params, workdir),
        "h5i_context_pack" => tool_context_pack(params, workdir),
        "h5i_context_search" => tool_context_search(params, workdir),
        "h5i_claims_add" => tool_claims_add(params, workdir),
        "h5i_claims_list" => tool_claims_list(params, workdir),
        "h5i_claims_prune" => tool_claims_prune(params, workdir),
        "h5i_summary_set" => tool_summary_set(params, workdir),
        "h5i_summary_get" => tool_summary_get(params, workdir),
        "h5i_summary_list" => tool_summary_list(params, workdir),
        "h5i_summary_prune" => tool_summary_prune(params, workdir),
        other => anyhow::bail!("Unknown tool: {}", other),
    }
}

// ── Resource handlers ─────────────────────────────────────────────────────────

/// Read a named MCP resource by URI.
pub fn read_resource(uri: &str, workdir: &Path) -> Result<Value> {
    match uri {
        "h5i://context/current" => {
            if !ctx::is_initialized(workdir) {
                return Ok(json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": "{\"initialized\":false}"
                    }]
                }));
            }
            let opts = ContextOpts {
                branch: None,
                commit_hash: None,
                show_log: true,
                log_offset: 0,
                metadata_segment: None,
                window: 5,
                depth: 2,
            };
            let context = ctx::gcc_context(workdir, &opts)?;
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string(&context)?
                }]
            }))
        }
        "h5i://context/snapshots" => {
            let snapshots = resource_context_snapshots(workdir);
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string(&snapshots)?
                }]
            }))
        }
        "h5i://log/recent" => {
            let repo = H5iRepository::open(workdir)?;
            let records = repo.get_log(10)?;
            Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string(&records)?
                }]
            }))
        }
        other => anyhow::bail!("Unknown resource URI: {}", other),
    }
}

/// Build a JSON array of available context snapshots by walking the
/// `snapshots/` subtree in `refs/h5i/context`.  Returns an empty array if
/// the workspace is uninitialised or no snapshots exist yet.
fn resource_context_snapshots(workdir: &Path) -> serde_json::Value {
    use git2::{ObjectType, Repository};

    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return json!([]),
    };
    let tip = match repo
        .find_reference(ctx::CTX_REF)
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
    {
        Some(c) => c,
        None => return json!([]),
    };
    let tree = match tip.tree() {
        Ok(t) => t,
        Err(_) => return json!([]),
    };
    let snap_entry = match tree
        .get_name("snapshots")
        .filter(|e| e.kind() == Some(ObjectType::Tree))
    {
        Some(e) => e,
        None => return json!([]),
    };
    let snap_tree = match repo.find_tree(snap_entry.id()) {
        Ok(t) => t,
        Err(_) => return json!([]),
    };

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for entry in snap_tree.iter() {
        if entry.kind() != Some(ObjectType::Blob) {
            continue;
        }
        let blob = match repo.find_blob(entry.id()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = match std::str::from_utf8(blob.content()) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Parse the key fields from the snapshot markdown.
        let mut linked_commit = String::new();
        let mut timestamp = String::new();
        let mut branch = String::new();
        let mut goal = String::new();
        for line in text.lines() {
            if line.starts_with("**Linked commit:**") {
                linked_commit = line
                    .split("**Linked commit:**")
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            } else if line.starts_with("**Timestamp:**") {
                timestamp = line
                    .split("**Timestamp:**")
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            } else if line.starts_with("**Branch:**") {
                branch = line
                    .split("**Branch:**")
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            } else if line.starts_with("**Goal:**") {
                goal = line
                    .split("**Goal:**")
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
            }
        }
        if !linked_commit.is_empty() {
            entries.push(json!({
                "sha": linked_commit,
                "timestamp": timestamp,
                "branch": branch,
                "goal": goal
            }));
        }
    }

    json!(entries)
}

// ── Resource subscriptions ────────────────────────────────────────────────────

/// Shared map: resource URI → last-seen serialised snapshot.
/// Removing a URI signals the watcher thread for that URI to exit.
type SubscriptionMap = Arc<Mutex<HashMap<String, String>>>;

/// Serialise a resource to a comparable string snapshot.
/// Returns an empty string on any error (e.g. repo not initialised) so callers
/// can skip sending spurious change notifications.
fn resource_snapshot(uri: &str, workdir: &Path) -> String {
    read_resource(uri, workdir)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// Register a subscription for `uri` and — if no watcher thread already exists
/// for this URI — spawn a background polling thread that pushes
/// `notifications/resources/updated` to `stdout` whenever the resource content
/// changes.
///
/// If the URI is already in `subs`, the existing watcher thread is reused and
/// the snapshot baseline is refreshed.
pub fn subscribe_resource(
    uri: String,
    workdir: PathBuf,
    subs: SubscriptionMap,
    stdout: Arc<Mutex<io::Stdout>>,
) {
    let snapshot = resource_snapshot(&uri, &workdir);

    {
        let mut map = subs.lock().unwrap();
        // If the URI is already subscribed, refresh its baseline and skip
        // spawning another watcher thread.
        if map.insert(uri.clone(), snapshot).is_some() {
            return;
        }
    }

    // Detached polling thread.  Exits when the URI is removed from `subs`.
    std::thread::spawn(move || {
        const POLL_SECS: u64 = 2;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));

            // Check if still subscribed and retrieve the last-known snapshot.
            let last = {
                let map = subs.lock().unwrap();
                match map.get(&uri) {
                    Some(s) => s.clone(),
                    None => return, // Unsubscribed — exit.
                }
            };

            let current = resource_snapshot(&uri, &workdir);

            // Skip empty snapshots (transient errors) and unchanged content.
            if current.is_empty() || current == last {
                continue;
            }

            // Persist the new snapshot before pushing so we don't re-notify
            // on the next poll if the client hasn't re-read yet.
            {
                let mut map = subs.lock().unwrap();
                if !map.contains_key(&uri) {
                    return; // Unsubscribed while computing snapshot.
                }
                map.insert(uri.clone(), current);
            }

            // Emit MCP notification.
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "notifications/resources/updated",
                "params": { "uri": &uri }
            });
            if let (Ok(msg), Ok(mut out)) =
                (serde_json::to_string(&notif), stdout.lock())
            {
                let _ = writeln!(out, "{}", msg);
                let _ = out.flush();
            }
        }
    });
}

// ── Request handler ───────────────────────────────────────────────────────────

/// Process one JSON-RPC request and return a response (or `None` for
/// notifications, which must not be answered).
pub fn handle_request(req: JsonRpcRequest, workdir: &Path) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        // ── Notifications (no response) ───────────────────────────────────────
        "notifications/initialized" | "notifications/cancelled" => None,

        // ── MCP lifecycle ─────────────────────────────────────────────────────
        "initialize" => Some(JsonRpcResponse::ok(
            req.id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {},
                    "resources": { "subscribe": true }
                },
                "serverInfo": {
                    "name": "h5i",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),

        // ── Tool discovery ────────────────────────────────────────────────────
        "tools/list" => Some(JsonRpcResponse::ok(
            req.id,
            json!({ "tools": tool_definitions() }),
        )),

        // ── Resource discovery ────────────────────────────────────────────────
        "resources/list" => Some(JsonRpcResponse::ok(
            req.id,
            json!({ "resources": resource_definitions() }),
        )),

        // ── Resource read ─────────────────────────────────────────────────────
        "resources/read" => {
            let uri = match req.params.get("uri").and_then(Value::as_str) {
                Some(u) => u,
                None => {
                    return Some(JsonRpcResponse::err(
                        req.id,
                        -32602,
                        "missing param: uri",
                    ))
                }
            };
            match read_resource(uri, workdir) {
                Ok(result) => Some(JsonRpcResponse::ok(req.id, result)),
                Err(e) => Some(JsonRpcResponse::err(req.id, -32603, e.to_string())),
            }
        }

        // ── Tool call ─────────────────────────────────────────────────────────
        "tools/call" => {
            let name = match req.params.get("name").and_then(Value::as_str) {
                Some(n) => n,
                None => {
                    return Some(JsonRpcResponse::err(
                        req.id,
                        -32602,
                        "missing param: name",
                    ))
                }
            };
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(name, &args, workdir) {
                Ok(result) => Some(JsonRpcResponse::ok(req.id, result)),
                // MCP spec: tool errors are content with isError:true, not
                // JSON-RPC errors, so the client can display them gracefully.
                Err(e) => Some(JsonRpcResponse::ok(
                    req.id,
                    json!({
                        "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                        "isError": true
                    }),
                )),
            }
        }

        // ── Utility ───────────────────────────────────────────────────────────
        "ping" => Some(JsonRpcResponse::ok(req.id, json!({}))),

        // ── Unknown ───────────────────────────────────────────────────────────
        other => Some(JsonRpcResponse::err(
            req.id,
            -32601,
            format!("Method not found: {}", other),
        )),
    }
}

// ── stdio transport ───────────────────────────────────────────────────────────

/// Run the MCP server on stdin/stdout.
///
/// Reads newline-delimited JSON-RPC 2.0 messages from stdin and writes
/// responses to stdout.  All log output goes to stderr so it does not
/// contaminate the protocol stream.
///
/// `resources/subscribe` and `resources/unsubscribe` are handled here (not in
/// `handle_request`) because they need access to the shared subscription map
/// and the `Arc`-wrapped stdout used by the watcher threads.
pub fn run_stdio(workdir: PathBuf) -> Result<()> {
    let stdin = io::stdin();
    // Wrap stdout in Arc<Mutex<>> so subscription watcher threads can write
    // notifications without racing with the main request loop.
    let stdout: Arc<Mutex<io::Stdout>> = Arc::new(Mutex::new(io::stdout()));
    let subs: SubscriptionMap = Arc::new(Mutex::new(HashMap::new()));

    // Known subscribable URIs — used to validate subscribe requests.
    const SUBSCRIBABLE: &[&str] = &["h5i://context/current", "h5i://log/recent"];

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(e) => {
                eprintln!("h5i-mcp: read error: {}", e);
                break;
            }
        };

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::err(None, -32700, format!("Parse error: {}", e));
                if let (Ok(msg), Ok(mut out)) = (serde_json::to_string(&resp), stdout.lock()) {
                    let _ = writeln!(out, "{}", msg);
                    let _ = out.flush();
                }
                continue;
            }
        };

        // Handle subscribe/unsubscribe before delegating to handle_request so
        // they have access to the subscription infrastructure.
        let resp_opt: Option<JsonRpcResponse> = match req.method.as_str() {
            "resources/subscribe" => {
                match req.params.get("uri").and_then(Value::as_str) {
                    None => Some(JsonRpcResponse::err(req.id, -32602, "missing param: uri")),
                    Some(uri) if !SUBSCRIBABLE.contains(&uri) => Some(JsonRpcResponse::err(
                        req.id,
                        -32602,
                        format!("not a subscribable resource: {}", uri),
                    )),
                    Some(uri) => {
                        subscribe_resource(
                            uri.to_string(),
                            workdir.clone(),
                            Arc::clone(&subs),
                            Arc::clone(&stdout),
                        );
                        Some(JsonRpcResponse::ok(req.id, json!({})))
                    }
                }
            }

            "resources/unsubscribe" => {
                match req.params.get("uri").and_then(Value::as_str) {
                    None => Some(JsonRpcResponse::err(req.id, -32602, "missing param: uri")),
                    Some(uri) => {
                        subs.lock().unwrap().remove(uri);
                        Some(JsonRpcResponse::ok(req.id, json!({})))
                    }
                }
            }

            _ => handle_request(req, &workdir),
        };

        if let Some(resp) = resp_opt {
            if let (Ok(msg), Ok(mut out)) = (serde_json::to_string(&resp), stdout.lock()) {
                let _ = writeln!(out, "{}", msg);
                let _ = out.flush();
            }
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Create a minimal git repo with a real initial commit so that HEAD and
    /// `H5iRepository::open` work.
    fn make_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let repo = git2::Repository::init(&path).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@test.com").unwrap();
        }
        // Write a file and make an initial commit so HEAD resolves.
        fs::write(path.join("hello.rs"), "fn main() {}").unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();

        (dir, path)
    }

    fn make_req(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: method.into(),
            params,
        }
    }

    // ── JSON-RPC response construction ────────────────────────────────────────

    #[test]
    fn response_ok_sets_result_clears_error() {
        let r = JsonRpcResponse::ok(Some(json!(1)), json!({"a": 1}));
        assert!(r.result.is_some());
        assert!(r.error.is_none());
        assert_eq!(r.jsonrpc, "2.0");
        assert_eq!(r.id, Some(json!(1)));
    }

    #[test]
    fn response_err_sets_error_clears_result() {
        let r = JsonRpcResponse::err(Some(json!(2)), -32600, "bad");
        assert!(r.result.is_none());
        assert!(r.error.is_some());
        let e = r.error.unwrap();
        assert_eq!(e.code, -32600);
        assert_eq!(e.message, "bad");
    }

    #[test]
    fn response_serialization_omits_null_fields() {
        let r = JsonRpcResponse::ok(None, json!(42));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"error\""), "error field must be absent");
        // id: None is also skipped
        assert!(!s.contains("\"id\""), "null id must be absent");
    }

    // ── Parse error handling ──────────────────────────────────────────────────

    #[test]
    fn parse_error_on_invalid_json() {
        let (_dir, path) = make_repo();
        // Simulate what the stdio loop does on a bad line.
        let bad = "not json at all {{";
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(bad);
        assert!(result.is_err());
        // The loop would emit a -32700 response.
        let resp = JsonRpcResponse::err(None, -32700, "Parse error");
        assert_eq!(resp.error.unwrap().code, -32700);
        let _ = path; // keep tempdir alive
    }

    // ── initialize ───────────────────────────────────────────────────────────

    #[test]
    fn initialize_returns_protocol_version_and_capabilities() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("initialize", json!({})), &path).unwrap();
        let r = resp.result.unwrap();
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["serverInfo"]["name"], "h5i");
        assert!(r["capabilities"]["tools"].is_object());
        assert!(r["capabilities"]["resources"].is_object());
    }

    // ── notifications ─────────────────────────────────────────────────────────

    #[test]
    fn notifications_return_no_response() {
        let (_dir, path) = make_repo();
        for method in &["notifications/initialized", "notifications/cancelled"] {
            let req = JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: None,
                method: method.to_string(),
                params: json!({}),
            };
            assert!(
                handle_request(req, &path).is_none(),
                "{} must return None",
                method
            );
        }
    }

    // ── tools/list ────────────────────────────────────────────────────────────

    #[test]
    fn tools_list_includes_all_expected_tools() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("tools/list", json!({})), &path).unwrap();
        let tools = resp.result.unwrap()["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();

        let expected = [
            "h5i_commit",
            "h5i_rewind",
            "h5i_notes_analyze",
            "h5i_log",
            "h5i_blame",
            "h5i_notes_show",
            "h5i_notes_uncertainty",
            "h5i_notes_coverage",
            "h5i_notes_review",
            "h5i_notes_churn",
            "h5i_context_init",
            "h5i_context_trace",
            "h5i_context_commit",
            "h5i_context_branch",
            "h5i_context_checkout",
            "h5i_context_merge",
            "h5i_context_show",
            "h5i_context_status",
            "h5i_context_knowledge",
            "h5i_context_restore",
            "h5i_context_diff",
            "h5i_context_relevant",
            "h5i_context_scan",
            "h5i_context_pack",
            "h5i_context_search",
            "h5i_claims_add",
            "h5i_claims_list",
            "h5i_claims_prune",
            "h5i_summary_set",
            "h5i_summary_get",
            "h5i_summary_list",
            "h5i_summary_prune",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing tool: {}", name);
        }
        assert_eq!(names.len(), expected.len(), "unexpected extra tools");
    }

    #[test]
    fn every_tool_has_description_and_object_schema() {
        let tools = tool_definitions();
        for tool in tools.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(
                !tool["description"].as_str().unwrap_or("").is_empty(),
                "{}: empty description",
                name
            );
            assert_eq!(
                tool["inputSchema"]["type"].as_str(),
                Some("object"),
                "{}: inputSchema.type must be 'object'",
                name
            );
        }
    }

    // ── resources/list ────────────────────────────────────────────────────────

    #[test]
    fn resources_list_includes_context_and_log() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("resources/list", json!({})), &path).unwrap();
        let resources = resp.result.unwrap()["resources"].clone();
        let uris: Vec<&str> = resources
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"h5i://context/current"));
        assert!(uris.contains(&"h5i://log/recent"));
    }

    // ── ping ──────────────────────────────────────────────────────────────────

    #[test]
    fn ping_returns_empty_object() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("ping", json!({})), &path).unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), json!({}));
    }

    // ── unknown method ────────────────────────────────────────────────────────

    #[test]
    fn unknown_method_returns_method_not_found() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("no/such/method", json!({})), &path).unwrap();
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32601);
        assert!(e.message.contains("not found"));
    }

    // ── tools/call: validation ────────────────────────────────────────────────

    #[test]
    fn tool_call_missing_name_returns_32602() {
        let (_dir, path) = make_repo();
        let resp = handle_request(
            make_req("tools/call", json!({"arguments": {}})),
            &path,
        )
        .unwrap();
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn tool_call_unknown_tool_returns_is_error() {
        let (_dir, path) = make_repo();
        let resp = handle_request(
            make_req(
                "tools/call",
                json!({"name": "nonexistent_tool", "arguments": {}}),
            ),
            &path,
        )
        .unwrap();
        // Unknown tool → isError:true in content, NOT a JSON-RPC error.
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["isError"], true);
    }

    // ── resources/read: validation ────────────────────────────────────────────

    #[test]
    fn resource_read_missing_uri_returns_32602() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("resources/read", json!({})), &path).unwrap();
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn resource_read_unknown_uri_returns_32603() {
        let (_dir, path) = make_repo();
        let resp = handle_request(
            make_req("resources/read", json!({"uri": "h5i://nope"})),
            &path,
        )
        .unwrap();
        assert_eq!(resp.error.unwrap().code, -32603);
    }

    // ── h5i_blame ─────────────────────────────────────────────────────────────

    #[test]
    fn blame_missing_file_param_is_error() {
        let (_dir, path) = make_repo();
        let result = tool_blame(&json!({}), &path);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("missing required param: file")
        );
    }

    // ── h5i_context_init ──────────────────────────────────────────────────────

    #[test]
    fn context_init_creates_workspace() {
        let (_dir, path) = make_repo();
        let r = tool_context_init(&json!({"goal": "build MCP server"}), &path);
        assert!(r.is_ok(), "{:?}", r);
        assert!(ctx::is_initialized(&path));
    }

    #[test]
    fn context_init_without_goal_still_succeeds() {
        let (_dir, path) = make_repo();
        let r = tool_context_init(&json!({}), &path);
        assert!(r.is_ok(), "{:?}", r);
        assert!(ctx::is_initialized(&path));
    }

    // ── h5i_context_trace ─────────────────────────────────────────────────────

    #[test]
    fn context_trace_auto_inits_and_echoes_entry() {
        let (_dir, path) = make_repo();
        assert!(!ctx::is_initialized(&path));
        let r =
            tool_context_trace(&json!({"kind": "OBSERVE", "content": "hello world"}), &path);
        assert!(r.is_ok(), "{:?}", r);
        assert!(ctx::is_initialized(&path));
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("OBSERVE"));
        assert!(text.contains("hello world"));
    }

    #[test]
    fn context_trace_missing_kind_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_trace(&json!({"content": "no kind here"}), &path);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("kind"));
    }

    #[test]
    fn context_trace_missing_content_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_trace(&json!({"kind": "ACT"}), &path);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("content"));
    }

    // ── h5i_context_commit ────────────────────────────────────────────────────

    #[test]
    fn context_commit_auto_inits_and_records_summary() {
        let (_dir, path) = make_repo();
        let r = tool_context_commit(&json!({"summary": "analyzed modules"}), &path);
        assert!(r.is_ok(), "{:?}", r);
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("analyzed modules"));
    }

    #[test]
    fn context_commit_missing_summary_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_commit(&json!({"detail": "some detail"}), &path);
        assert!(r.is_err());
    }

    #[test]
    fn context_commit_uses_detail_when_provided() {
        let (_dir, path) = make_repo();
        let r = tool_context_commit(
            &json!({"summary": "summary text", "detail": "detailed description"}),
            &path,
        );
        assert!(r.is_ok(), "{:?}", r);
    }

    // ── h5i_context_branch ────────────────────────────────────────────────────

    #[test]
    fn context_branch_creates_branch_in_workspace() {
        let (_dir, path) = make_repo();
        tool_context_init(&json!({"goal": "test"}), &path).unwrap();
        let r = tool_context_branch(
            &json!({"name": "experiment/alt", "purpose": "try alternative"}),
            &path,
        );
        assert!(r.is_ok(), "{:?}", r);
        let branches = ctx::list_branches(&path);
        assert!(branches.contains(&"experiment/alt".to_string()));
    }

    #[test]
    fn context_branch_missing_name_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_branch(&json!({"purpose": "no name"}), &path);
        assert!(r.is_err());
    }

    #[test]
    fn context_branch_auto_inits() {
        let (_dir, path) = make_repo();
        assert!(!ctx::is_initialized(&path));
        let r = tool_context_branch(&json!({"name": "feature/x"}), &path);
        assert!(r.is_ok(), "{:?}", r);
        assert!(ctx::is_initialized(&path));
    }

    // ── h5i_context_checkout ──────────────────────────────────────────────────

    #[test]
    fn context_checkout_switches_active_branch() {
        let (_dir, path) = make_repo();
        tool_context_init(&json!({"goal": "test"}), &path).unwrap();
        tool_context_branch(&json!({"name": "feature/y"}), &path).unwrap();
        // currently on feature/y after branch creation; switch back to main
        let r = tool_context_checkout(&json!({"name": "main"}), &path);
        assert!(r.is_ok(), "{:?}", r);
        assert_eq!(ctx::current_branch(&path), "main");
    }

    #[test]
    fn context_checkout_missing_name_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_checkout(&json!({}), &path);
        assert!(r.is_err());
    }

    // ── h5i_context_merge ─────────────────────────────────────────────────────

    #[test]
    fn context_merge_missing_branch_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_context_merge(&json!({}), &path);
        assert!(r.is_err());
    }

    // ── h5i_context_show ──────────────────────────────────────────────────────

    #[test]
    fn context_show_not_initialized_returns_message() {
        let (_dir, path) = make_repo();
        let r = tool_context_show(&json!({}), &path);
        assert!(r.is_ok());
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["initialized"], false);
    }

    #[test]
    fn context_show_returns_gcc_context_json() {
        let (_dir, path) = make_repo();
        tool_context_init(&json!({"goal": "show goal"}), &path).unwrap();
        let r = tool_context_show(&json!({"window": 2, "trace": true}), &path);
        assert!(r.is_ok(), "{:?}", r);
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["project_goal"], "show goal");
        assert!(v["current_branch"].is_string());
        assert!(v["active_branches"].is_array());
    }

    // ── h5i_context_status ────────────────────────────────────────────────────

    #[test]
    fn context_status_not_initialized_returns_message() {
        let (_dir, path) = make_repo();
        let r = tool_context_status(&json!({}), &path);
        assert!(r.is_ok());
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["initialized"], false);
    }

    #[test]
    fn context_status_after_init_returns_structured_json() {
        let (_dir, path) = make_repo();
        tool_context_init(&json!({"goal": "status test"}), &path).unwrap();
        let r = tool_context_status(&json!({}), &path);
        assert!(r.is_ok(), "{:?}", r);
        let text = r.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["initialized"], true);
        assert_eq!(v["current_branch"], "main");
        assert!(v["branch_count"].as_u64().unwrap() >= 1);
    }

    // ── resources/read ────────────────────────────────────────────────────────

    #[test]
    fn resource_context_current_not_initialized_returns_false_flag() {
        let (_dir, path) = make_repo();
        let r = read_resource("h5i://context/current", &path);
        assert!(r.is_ok(), "{:?}", r);
        let text = r.unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["initialized"], false);
    }

    #[test]
    fn resource_context_current_initialized_returns_gcc_context() {
        let (_dir, path) = make_repo();
        tool_context_init(&json!({"goal": "resource goal"}), &path).unwrap();
        let r = read_resource("h5i://context/current", &path);
        assert!(r.is_ok(), "{:?}", r);
        let val = r.unwrap();
        assert_eq!(val["contents"][0]["uri"], "h5i://context/current");
        assert_eq!(val["contents"][0]["mimeType"], "application/json");
        // The text should parse as GccContext
        let text = val["contents"][0]["text"].as_str().unwrap();
        let ctx_val: Value = serde_json::from_str(text).unwrap();
        assert_eq!(ctx_val["project_goal"], "resource goal");
    }

    #[test]
    fn resource_unknown_uri_is_error() {
        let (_dir, path) = make_repo();
        let r = read_resource("h5i://nonexistent/path", &path);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("Unknown resource"));
    }

    // ── Full context workflow ─────────────────────────────────────────────────

    #[test]
    fn full_context_workflow() {
        let (_dir, path) = make_repo();

        // init
        tool_context_init(&json!({"goal": "implement MCP server"}), &path).unwrap();

        // trace multiple steps
        for (kind, content) in &[
            ("OBSERVE", "codebase has server.rs already"),
            ("THINK", "stdio is simpler than HTTP for a CLI tool"),
            ("ACT", "created src/mcp.rs"),
        ] {
            tool_context_trace(&json!({"kind": kind, "content": content}), &path).unwrap();
        }

        // checkpoint
        tool_context_commit(
            &json!({"summary": "planned MCP layout", "detail": "identified all modules"}),
            &path,
        )
        .unwrap();

        // branch for an experiment
        tool_context_branch(
            &json!({"name": "experiment/http", "purpose": "try HTTP transport"}),
            &path,
        )
        .unwrap();
        tool_context_trace(
            &json!({"kind": "THINK", "content": "HTTP adds complexity for no gain"}),
            &path,
        )
        .unwrap();

        // return to main
        tool_context_checkout(&json!({"name": "main"}), &path).unwrap();
        assert_eq!(ctx::current_branch(&path), "main");

        // verify show
        let show = tool_context_show(&json!({"trace": true, "window": 5}), &path).unwrap();
        let text = show["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["project_goal"], "implement MCP server");
        assert!(v["active_branches"].as_array().unwrap().len() >= 2);

        // verify status
        let status = tool_context_status(&json!({}), &path).unwrap();
        let s_text = status["content"][0]["text"].as_str().unwrap();
        let sv: Value = serde_json::from_str(s_text).unwrap();
        assert_eq!(sv["initialized"], true);
        assert_eq!(sv["current_branch"], "main");
        assert!(sv["branch_count"].as_u64().unwrap() >= 2);

        // verify resource is consistent
        let res = read_resource("h5i://context/current", &path).unwrap();
        let res_text = res["contents"][0]["text"].as_str().unwrap();
        let rv: Value = serde_json::from_str(res_text).unwrap();
        assert_eq!(rv["project_goal"], "implement MCP server");
    }

    // ── resources/subscribe ───────────────────────────────────────────────────

    #[test]
    fn initialize_advertises_subscribe_capability() {
        let (_dir, path) = make_repo();
        let resp = handle_request(make_req("initialize", json!({})), &path).unwrap();
        let caps = &resp.result.unwrap()["capabilities"];
        assert_eq!(
            caps["resources"]["subscribe"].as_bool(),
            Some(true),
            "capabilities.resources.subscribe must be true"
        );
    }

    #[test]
    fn subscribe_known_uri_returns_empty_ok() {
        let (_dir, path) = make_repo();
        let subs: SubscriptionMap = Arc::new(Mutex::new(HashMap::new()));
        let stdout = Arc::new(Mutex::new(io::stdout()));

        subscribe_resource(
            "h5i://log/recent".to_string(),
            path.clone(),
            Arc::clone(&subs),
            Arc::clone(&stdout),
        );

        // URI must be registered in the map immediately.
        let map = subs.lock().unwrap();
        assert!(
            map.contains_key("h5i://log/recent"),
            "URI must be in subscription map after subscribe"
        );
    }

    #[test]
    fn subscribe_idempotent_on_second_call() {
        let (_dir, path) = make_repo();
        let subs: SubscriptionMap = Arc::new(Mutex::new(HashMap::new()));
        let stdout = Arc::new(Mutex::new(io::stdout()));

        // First subscription.
        subscribe_resource(
            "h5i://log/recent".to_string(),
            path.clone(),
            Arc::clone(&subs),
            Arc::clone(&stdout),
        );
        let snap1 = subs.lock().unwrap().get("h5i://log/recent").cloned().unwrap();

        // Second subscription — should not panic or duplicate entries.
        subscribe_resource(
            "h5i://log/recent".to_string(),
            path.clone(),
            Arc::clone(&subs),
            Arc::clone(&stdout),
        );
        let snap2 = subs.lock().unwrap().get("h5i://log/recent").cloned().unwrap();

        // Snapshot may be refreshed but URI still present exactly once.
        assert_eq!(snap1, snap2, "snapshot should be stable for unchanged repo");
        assert_eq!(
            subs.lock().unwrap().len(),
            1,
            "only one entry per URI"
        );
    }

    #[test]
    fn unsubscribe_removes_uri_from_map() {
        let (_dir, path) = make_repo();
        let subs: SubscriptionMap = Arc::new(Mutex::new(HashMap::new()));
        let stdout = Arc::new(Mutex::new(io::stdout()));

        subscribe_resource(
            "h5i://log/recent".to_string(),
            path.clone(),
            Arc::clone(&subs),
            Arc::clone(&stdout),
        );
        assert!(subs.lock().unwrap().contains_key("h5i://log/recent"));

        // Remove via direct map manipulation (mirrors what run_stdio does for
        // resources/unsubscribe).
        subs.lock().unwrap().remove("h5i://log/recent");
        assert!(
            !subs.lock().unwrap().contains_key("h5i://log/recent"),
            "URI must be gone after unsubscribe"
        );
    }

    #[test]
    fn resource_snapshot_returns_nonempty_for_known_uris() {
        let (_dir, path) = make_repo();
        // h5i://log/recent should always work (HEAD exists from make_repo).
        let snap = resource_snapshot("h5i://log/recent", &path);
        assert!(!snap.is_empty(), "snapshot must not be empty for valid repo");
    }

    #[test]
    fn resource_snapshot_returns_empty_for_unknown_uri() {
        let (_dir, path) = make_repo();
        let snap = resource_snapshot("h5i://does/not/exist", &path);
        assert!(snap.is_empty(), "unknown URI must yield empty snapshot");
    }

    // ── h5i_claims_add ────────────────────────────────────────────────────────

    #[test]
    fn claims_add_records_claim_with_evidence() {
        let (_dir, path) = make_repo();
        let r = tool_claims_add(
            &json!({
                "text": "hello.rs has a trivial fn main",
                "paths": ["hello.rs"],
            }),
            &path,
        );
        assert!(r.is_ok(), "{:?}", r);
        let body: Value = serde_json::from_str(
            r.unwrap()["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(body["text"], "hello.rs has a trivial fn main");
        assert_eq!(body["evidence_paths"], json!(["hello.rs"]));
        assert!(body["id"].as_str().unwrap().len() >= 8);
        assert!(!body["evidence_oid"].as_str().unwrap().is_empty());
    }

    #[test]
    fn claims_add_missing_text_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_claims_add(&json!({"paths": ["hello.rs"]}), &path);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("text"));
    }

    #[test]
    fn claims_add_missing_paths_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_claims_add(&json!({"text": "x"}), &path);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("paths"));
    }

    #[test]
    fn claims_add_empty_paths_array_is_error() {
        let (_dir, path) = make_repo();
        let empty: Vec<String> = vec![];
        let r = tool_claims_add(
            &json!({"text": "x", "paths": empty}),
            &path,
        );
        assert!(r.is_err());
    }

    #[test]
    fn claims_add_untracked_path_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_claims_add(
            &json!({"text": "x", "paths": ["nope.rs"]}),
            &path,
        );
        assert!(r.is_err());
    }

    // ── h5i_claims_list ───────────────────────────────────────────────────────

    #[test]
    fn claims_list_empty_repo_returns_zero_counts() {
        let (_dir, path) = make_repo();
        let r = tool_claims_list(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["live"], 0);
        assert_eq!(body["stale"], 0);
        assert_eq!(body["claims"], json!([]));
    }

    #[test]
    fn claims_list_reports_live_after_add() {
        let (_dir, path) = make_repo();
        tool_claims_add(
            &json!({"text": "main lives in hello.rs", "paths": ["hello.rs"]}),
            &path,
        )
        .unwrap();
        let r = tool_claims_list(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["live"], 1);
        assert_eq!(body["stale"], 0);
        let claims_arr = body["claims"].as_array().unwrap();
        assert_eq!(claims_arr.len(), 1);
        assert_eq!(claims_arr[0]["status"], "live");
        assert_eq!(claims_arr[0]["text"], "main lives in hello.rs");
    }

    #[test]
    fn claims_list_reports_stale_after_evidence_change() {
        let (_dir, path) = make_repo();
        tool_claims_add(
            &json!({"text": "hello.rs is a stub", "paths": ["hello.rs"]}),
            &path,
        )
        .unwrap();
        // Edit hello.rs and commit — evidence_oid now differs.
        fs::write(path.join("hello.rs"), "fn main() { println!(\"changed\"); }").unwrap();
        let repo = git2::Repository::open(&path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "edit", &tree, &[&parent])
            .unwrap();

        let r = tool_claims_list(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["live"], 0);
        assert_eq!(body["stale"], 1);
        assert_eq!(body["claims"][0]["status"], "stale");
    }

    // ── h5i_claims_prune ──────────────────────────────────────────────────────

    #[test]
    fn claims_prune_reports_zero_when_all_live() {
        let (_dir, path) = make_repo();
        tool_claims_add(
            &json!({"text": "hello.rs is live", "paths": ["hello.rs"]}),
            &path,
        )
        .unwrap();
        let r = tool_claims_prune(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["removed"], 0);
    }

    #[test]
    fn claims_prune_removes_only_stale() {
        let (_dir, path) = make_repo();
        // Add a second file + commit, then two claims (one per file).
        fs::write(path.join("other.rs"), "fn other() {}").unwrap();
        let repo = git2::Repository::open(&path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("other.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add other", &tree, &[&parent])
            .unwrap();

        tool_claims_add(
            &json!({"text": "hello claim", "paths": ["hello.rs"]}),
            &path,
        )
        .unwrap();
        tool_claims_add(
            &json!({"text": "other claim", "paths": ["other.rs"]}),
            &path,
        )
        .unwrap();

        // Invalidate only the hello claim.
        fs::write(path.join("hello.rs"), "fn main() { /* edit */ }").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "edit hello", &tree, &[&parent])
            .unwrap();

        let r = tool_claims_prune(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["removed"], 1);

        // Remaining claim should be the live one.
        let list = tool_claims_list(&json!({}), &path).unwrap();
        let list_body: Value =
            serde_json::from_str(list["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(list_body["live"], 1);
        assert_eq!(list_body["stale"], 0);
        assert_eq!(list_body["claims"][0]["text"], "other claim");
    }

    // ── tools/list: claims present ────────────────────────────────────────────

    #[test]
    fn tools_list_includes_claims_tools() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"h5i_claims_add"));
        assert!(names.contains(&"h5i_claims_list"));
        assert!(names.contains(&"h5i_claims_prune"));
    }

    // ── h5i_summary_set / get / list / prune ─────────────────────────────────

    #[test]
    fn summary_set_records_summary_pinned_to_blob() {
        let (_dir, path) = make_repo();
        let r = tool_summary_set(
            &json!({"path": "hello.rs", "text": "Trivial Rust entry point."}),
            &path,
        );
        assert!(r.is_ok(), "{:?}", r);
        let body: Value = serde_json::from_str(
            r.unwrap()["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(body["path"], "hello.rs");
        assert_eq!(body["text_chars"], "Trivial Rust entry point.".len());
        assert!(!body["blob_oid"].as_str().unwrap().is_empty());
    }

    #[test]
    fn summary_set_missing_path_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_summary_set(&json!({"text": "x"}), &path);
        assert!(r.is_err());
    }

    #[test]
    fn summary_set_missing_text_is_error() {
        let (_dir, path) = make_repo();
        let r = tool_summary_set(&json!({"path": "hello.rs"}), &path);
        assert!(r.is_err());
    }

    #[test]
    fn summary_get_returns_text_after_set() {
        let (_dir, path) = make_repo();
        tool_summary_set(
            &json!({"path": "hello.rs", "text": "summary body"}),
            &path,
        )
        .unwrap();
        let r = tool_summary_get(&json!({"path": "hello.rs"}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["found"], true);
        assert_eq!(body["text"], "summary body");
    }

    #[test]
    fn summary_get_returns_not_found_for_unsummarised_blob() {
        let (_dir, path) = make_repo();
        let r = tool_summary_get(&json!({"path": "hello.rs"}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["found"], false);
        assert!(body["message"].as_str().unwrap().contains("h5i_summary_set"));
    }

    #[test]
    fn summary_list_reports_with_and_without_counts() {
        let (_dir, path) = make_repo();
        // make_repo() seeds with "hello.rs" only.
        let r = tool_summary_list(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["total"], 1);
        assert_eq!(body["with_count"], 0);
        assert_eq!(body["without_count"], 1);

        tool_summary_set(
            &json!({"path": "hello.rs", "text": "hi"}),
            &path,
        )
        .unwrap();
        let r2 = tool_summary_list(&json!({}), &path).unwrap();
        let body2: Value =
            serde_json::from_str(r2["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body2["with_count"], 1);
        assert_eq!(body2["without_count"], 0);
    }

    #[test]
    fn summary_prune_drops_unreachable_blobs() {
        let (_dir, path) = make_repo();
        tool_summary_set(
            &json!({"path": "hello.rs", "text": "v1 summary"}),
            &path,
        )
        .unwrap();

        // Edit + commit so the v1 blob is no longer reachable.
        fs::write(path.join("hello.rs"), "fn main() { let _ = 2; }").unwrap();
        let repo = git2::Repository::open(&path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("hello.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "edit", &tree, &[&parent])
            .unwrap();

        let r = tool_summary_prune(&json!({}), &path).unwrap();
        let body: Value =
            serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["removed"], 1);
    }

    #[test]
    fn tools_list_includes_summary_tools() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for n in [
            "h5i_summary_set",
            "h5i_summary_get",
            "h5i_summary_list",
            "h5i_summary_prune",
        ] {
            assert!(names.contains(&n), "missing {n}");
        }
    }
}
