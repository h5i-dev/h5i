//! `h5i push` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(remote: String, branch: Option<String>, all_branches: bool) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;

            // Resolve which branch's material to push. Scoping to the current
            // branch is the DEFAULT (like `git push`); `--all-branches` opts out.
            //   --all-branches  → None (push every branch's material).
            //   --branch <name> → that explicit branch.
            //   --branch (bare) / omitted → the current git branch.
            let ctx_scope: Option<String> = if all_branches {
                None
            } else {
                let resolved = match branch {
                    Some(name) if !name.is_empty() => name,
                    _ => h5i_core::ctx::current_git_branch(&workdir),
                };
                if let Err(e) = h5i_core::cli_routing::validate_ctx_branch_name(&resolved) {
                    anyhow::bail!("invalid --branch: {e}");
                }
                Some(resolved)
            };

            println!(
                "{} {} to {}",
                STEP,
                style("Pushing all h5i refs").cyan().bold(),
                style(&remote).yellow()
            );
            if let Some(b) = &ctx_scope {
                println!(
                    "  {} scoped to branch {} — context + notes + objects + msg + env for this \
                     branch only (ast/memory push in full; use {} for every branch)",
                    style("•").dim(),
                    style(b).cyan(),
                    style("--all-branches").bold(),
                );
            } else {
                println!(
                    "  {} {} — every branch's material",
                    style("•").dim(),
                    style("--all-branches").bold(),
                );
            }

            use std::io::Write as _;

            // Pre-check whether a ref exists locally before invoking `git push`.
            // Skipping a missing ref with our own warning avoids two lines of
            // git stderr noise ("error: src refspec ... does not match any" +
            // "error: failed to push some refs") for the expected case where
            // the user simply hasn't generated that artifact yet.
            let ref_exists = |refname: &str| -> bool {
                std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "--quiet", refname])
                    .current_dir(&workdir)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };

            // Push one h5i ref. On missing ref, prints a yellow warning with
            // the hint command. On real push failure, lets git's stderr
            // through unchanged. Returns true iff the push actually ran and
            // succeeded — used downstream to gate the "Tip:" footer.
            let try_push = |refname: &str,
                            missing_hint: console::StyledObject<&str>,
                            missing_reason: &str|
             -> anyhow::Result<bool> {
                print!("  {} {} … ", style("→").dim(), style(refname).yellow());
                std::io::stdout().flush()?;
                if !ref_exists(refname) {
                    println!(
                        "{} ({} — run {})",
                        style("skipped").yellow(),
                        missing_reason,
                        missing_hint
                    );
                    return Ok(false);
                }
                let refspec = format!("+{}:{}", refname, refname);
                let status = std::process::Command::new("git")
                    .args(["push", &remote, &refspec])
                    .current_dir(&workdir)
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                if status.success() {
                    println!("{}", style("ok").green());
                    Ok(true)
                } else {
                    println!("{}", style("failed").red());
                    Ok(false)
                }
            };

            // Branch-scoped push of an aggregate ref (notes / objects). Unlike
            // the one-ref-per-branch context layout, these refs are single
            // aggregate object graphs shared by every branch, so we cannot just
            // force-push a filtered subset — that would delete the remote's data
            // for all other branches. Instead we fetch the remote's current ref
            // into a temp ref and union *only this branch's* entries onto it,
            // then push the result (a fast-forward). Mirrors git-push semantics:
            // additive, scoped to the branch, never destructive to others.
            let git_run = |args: &[&str]| -> std::io::Result<std::process::Output> {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&workdir)
                    .output()
            };

            // Notes: union the remote's notes with the local notes for every
            // commit reachable from the branch.
            let scoped_push_notes = |branch: &str| -> anyhow::Result<bool> {
                let temp = "refs/h5i/_scoped_push/notes";
                let _ = git_run(&["update-ref", "-d", temp]);
                print!(
                    "  {} {} … ",
                    style("→").dim(),
                    style("refs/h5i/notes").yellow()
                );
                std::io::stdout().flush()?;
                // Seed temp with the remote's notes (absent on first push: ok).
                let _ = git_run(&[
                    "fetch",
                    "--no-write-fetch-head",
                    &remote,
                    &format!("+refs/h5i/notes:{temp}"),
                ]);
                // Commit set reachable from the branch. Prefer the branch ref;
                // fall back to HEAD so a detached checkout (common in CI) still
                // scopes to the checked-out history rather than pushing nothing.
                let rev_list = |rev: &str| -> std::collections::HashSet<String> {
                    match git_run(&["rev-list", rev]) {
                        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        _ => std::collections::HashSet::new(),
                    }
                };
                let mut reachable = rev_list(&format!("refs/heads/{branch}"));
                if reachable.is_empty() {
                    reachable = rev_list("HEAD");
                }
                let g2 = git2::Repository::open(&workdir)
                    .map_err(|e| anyhow::anyhow!("open git repo: {e}"))?;
                let copied =
                    h5i_core::repository::copy_scoped_notes_onto(&g2, &reachable, temp)
                        .map_err(|e| anyhow::anyhow!("scope notes: {e}"))?;
                let temp_exists = git_run(&["rev-parse", "--verify", "--quiet", temp])
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !temp_exists {
                    println!(
                        "{} (no provenance for branch {})",
                        style("skipped").yellow(),
                        style(branch).cyan()
                    );
                    return Ok(false);
                }
                let status = git_run(&["push", &remote, &format!("{temp}:refs/h5i/notes")])?;
                let _ = git_run(&["update-ref", "-d", temp]);
                if status.status.success() {
                    println!(
                        "{} ({} note{} for {})",
                        style("ok").green(),
                        copied,
                        if copied == 1 { "" } else { "s" },
                        style(branch).cyan()
                    );
                    Ok(true)
                } else {
                    println!("{}", style("failed").red());
                    eprint!("{}", String::from_utf8_lossy(&status.stderr));
                    Ok(false)
                }
            };

            // Objects: union the remote's manifest log with the local manifests
            // captured on the branch (the `branch` field of each record).
            // Generic scoped non-destructive merge-push for an aggregate log ref
            // (objects / msg / env-meta). `build` reads the local ref + the
            // fetched remote `base` and returns the merged commit to push (remote
            // ∪ this branch's records), or None when there is nothing for the
            // branch. The push is a fast-forward off the remote tip — never a
            // force of a filtered subset — so other branches' data survives.
            type ScopedBuild = dyn Fn(
                &git2::Repository,
                &str,
                Option<git2::Oid>,
            ) -> Result<Option<git2::Oid>, h5i_core::error::H5iError>;
            let scoped_merge_push = |branch: &str,
                                     refname: &str,
                                     no_data: &str,
                                     build: &ScopedBuild|
             -> anyhow::Result<bool> {
                let leaf = refname.rsplit('/').next().unwrap_or("ref");
                let temp = format!("refs/h5i/_scoped_push/{leaf}");
                let _ = git_run(&["update-ref", "-d", &temp]);
                print!("  {} {} … ", style("→").dim(), style(refname).yellow());
                std::io::stdout().flush()?;
                let _ = git_run(&[
                    "fetch",
                    "--no-write-fetch-head",
                    &remote,
                    &format!("+{refname}:{temp}"),
                ]);
                let base_oid = git_run(&["rev-parse", "--verify", "--quiet", &temp])
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        git2::Oid::from_str(String::from_utf8_lossy(&o.stdout).trim()).ok()
                    });
                let g2 = git2::Repository::open(&workdir)
                    .map_err(|e| anyhow::anyhow!("open git repo: {e}"))?;
                let merged = build(&g2, branch, base_oid)
                    .map_err(|e| anyhow::anyhow!("scope {refname}: {e}"))?;
                let Some(oid) = merged else {
                    let _ = git_run(&["update-ref", "-d", &temp]);
                    println!("{} ({no_data})", style("skipped").yellow());
                    return Ok(false);
                };
                let _ = git_run(&["update-ref", &temp, &oid.to_string()]);
                let status = git_run(&["push", &remote, &format!("{temp}:{refname}")])?;
                let _ = git_run(&["update-ref", "-d", &temp]);
                if status.status.success() {
                    println!("{} (scoped to {})", style("ok").green(), style(branch).cyan());
                    Ok(true)
                } else {
                    println!("{}", style("failed").red());
                    eprint!("{}", String::from_utf8_lossy(&status.stderr));
                    Ok(false)
                }
            };

            // Push h5i notes (AI provenance, test metrics, causal links).
            // Scoped to the branch when --branch is given; else the whole ref.
            let notes_pushed = if let Some(b) = &ctx_scope {
                scoped_push_notes(b)?
            } else {
                try_push(
                    "refs/h5i/notes",
                    style("h5i commit").bold(),
                    "no AI-provenance commits yet",
                )?
            };

            // Push memory ref (Claude memory snapshots)
            try_push(
                memory::MEMORY_REF,
                style("h5i memory snapshot").bold(),
                "no memory snapshots yet",
            )?;

            // Push context workspace.
            //
            // Post-redesign: one ref per context branch under
            // `refs/h5i/context/<name>`. Unscoped (the default) ships every
            // branch's DAG with a single wildcard refspec, and also pushes the
            // legacy single ref (`refs/h5i/context`) + migration backup
            // (`refs/h5i/context-legacy`) for older receivers / rollback
            // diagnosis. `--branch <b>` instead narrows the push to that
            // branch's `refs/h5i/context/<b>` so pushing one code branch does
            // not leak the reasoning DAGs of unrelated branches; the legacy
            // whole-workspace refs are intentionally skipped when scoped.
            if let Some(b) = &ctx_scope {
                let scoped_ref = h5i_core::ctx::branch_ref(b);
                print!("  {} {} … ", style("→").dim(), style(&scoped_ref).yellow());
                std::io::stdout().flush()?;
                if !ref_exists(&scoped_ref) {
                    println!(
                        "{} (no context workspace for branch {} — run {})",
                        style("skipped").yellow(),
                        style(b).cyan(),
                        style("h5i context init").bold(),
                    );
                } else {
                    let refspec = h5i_core::cli_routing::context_push_refspec(Some(b));
                    let status = std::process::Command::new("git")
                        .args(["push", &remote, &refspec])
                        .current_dir(&workdir)
                        .status()
                        .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                    println!(
                        "{}",
                        if status.success() {
                            style("ok").green()
                        } else {
                            style("failed").red()
                        }
                    );
                    if !status.success() && remote_has_legacy_context_ref(&remote, &workdir) {
                        print_legacy_context_remediation(&remote);
                    }
                }
            } else {
                let any_per_branch_ctx = std::process::Command::new("git")
                    .args([
                        "for-each-ref",
                        "--count=1",
                        "--format=%(refname)",
                        "refs/h5i/context/",
                    ])
                    .current_dir(&workdir)
                    .output()
                    .map(|o| !o.stdout.is_empty())
                    .unwrap_or(false);
                if any_per_branch_ctx {
                    print!(
                        "  {} {} … ",
                        style("→").dim(),
                        style("refs/h5i/context/*").yellow()
                    );
                    std::io::stdout().flush()?;
                    let status = std::process::Command::new("git")
                        .args([
                            "push",
                            &remote,
                            &h5i_core::cli_routing::context_push_refspec(None),
                        ])
                        .current_dir(&workdir)
                        .status()
                        .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                    println!(
                        "{}",
                        if status.success() {
                            style("ok").green()
                        } else {
                            style("failed").red()
                        }
                    );
                    // The single most common cause of this failure is a remote
                    // that still hosts the pre-redesign single
                    // `refs/h5i/context` ref, which collides with the per-branch
                    // directory. Detect it and point at the one-shot fix instead
                    // of leaving a raw git error.
                    if !status.success() && remote_has_legacy_context_ref(&remote, &workdir) {
                        print_legacy_context_remediation(&remote);
                    }
                } else {
                    println!(
                        "  {} {} … {} (no context workspace yet — run {})",
                        style("→").dim(),
                        style("refs/h5i/context/*").yellow(),
                        style("skipped").yellow(),
                        style("h5i context init").bold(),
                    );
                }
                if ref_exists("refs/h5i/context") {
                    try_push(
                        "refs/h5i/context",
                        style("(legacy)").dim(),
                        "(no legacy ref)",
                    )?;
                }
                if ref_exists("refs/h5i/context-legacy") {
                    try_push(
                        "refs/h5i/context-legacy",
                        style("(migration backup)").dim(),
                        "(no migration backup)",
                    )?;
                }
            }

            // Push the cross-agent message log (refs/h5i/msg). Scoped to the
            // branch's conversation (messages auto-tagged with the branch) when
            // --branch is given; else the whole log. The roster always travels.
            if let Some(b) = &ctx_scope {
                scoped_merge_push(
                    b,
                    msg::MSG_REF,
                    "no messages for this branch",
                    &h5i_core::msg::build_branch_scoped_merge,
                )?;
            } else {
                try_push(
                    msg::MSG_REF,
                    style("h5i msg send").bold(),
                    "no messages yet",
                )?;
            }

            // Push the token-reduction manifest log (refs/h5i/objects).
            // Only the small pointer records travel; raw blobs stay local
            // until a remote object backend exists (git-lfs style). Scoped to
            // the branch's captures when --branch is given; else the whole ref.
            if let Some(b) = &ctx_scope {
                // Also carry the evidence captures of envs forked from this
                // branch (their objects are tagged with the env's own branch, so
                // a plain branch match would miss them).
                let env_ids = git2::Repository::open(&workdir)
                    .ok()
                    .map(|r| h5i_core::env::local_env_ids_for_branch(&r, b))
                    .unwrap_or_default();
                let build_objects = move |repo: &git2::Repository,
                                          branch: &str,
                                          base: Option<git2::Oid>| {
                    h5i_core::objects::build_branch_scoped_merge(repo, branch, &env_ids, base)
                };
                scoped_merge_push(
                    b,
                    h5i_core::objects::OBJECTS_REF,
                    "no captures for this branch",
                    &build_objects,
                )?;
            } else {
                try_push(
                    h5i_core::objects::OBJECTS_REF,
                    style("h5i capture run").bold(),
                    "no captured objects yet",
                )?;
            }

            // Push the shareable env state (manifests + policies + event log).
            // Scoped to the envs forked from the branch (manifest parent_branch)
            // when --branch is given; else the whole ref.
            if let Some(b) = &ctx_scope {
                scoped_merge_push(
                    b,
                    h5i_core::env::ENV_REF,
                    "no environments for this branch",
                    &h5i_core::env::build_branch_scoped_merge,
                )?;
            } else {
                try_push(
                    h5i_core::env::ENV_REF,
                    style("h5i env create").bold(),
                    "no environments yet",
                )?;
            }
            let git_out = |args: &[&str]| {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&workdir)
                    .output()
            };
            // Push the env CODE branches onto the hidden refs/h5i/env/code/*
            // namespace so a reviewer on another clone can diff/apply, without
            // the branches ever appearing in the remote's UI.
            let any_env_branch = git_out(&[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "refs/heads/h5i/env/",
            ])
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
            if any_env_branch {
                print!(
                    "  {} {} … ",
                    style("→").dim(),
                    style("refs/h5i/env/code/*").yellow()
                );
                std::io::stdout().flush()?;
                // When scoped, push only the code branches of envs forked from
                // this branch (remapped onto the hidden code namespace); else the
                // wildcard carries every env branch.
                let refspecs: Vec<String> = if let Some(b) = &ctx_scope {
                    git2::Repository::open(&workdir)
                        .ok()
                        .map(|r| h5i_core::env::scoped_code_branch_refs(&r, b))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|full| {
                            full.strip_prefix("refs/heads/h5i/env/")
                                .map(|suffix| format!("+{full}:refs/h5i/env/code/{suffix}"))
                        })
                        .collect()
                } else {
                    vec![ENV_CODE_PUSH_REFSPEC.to_string()]
                };
                if refspecs.is_empty() {
                    println!(
                        "{} (no env code for this branch)",
                        style("skipped").yellow()
                    );
                } else {
                    let mut args: Vec<String> = vec!["push".into(), remote.clone()];
                    args.extend(refspecs);
                    let status = std::process::Command::new("git")
                        .args(&args)
                        .current_dir(&workdir)
                        .status()
                        .map_err(|e| anyhow::anyhow!("Failed to invoke git push: {e}"))?;
                    println!(
                        "{}",
                        if status.success() {
                            style("ok").green()
                        } else {
                            style("failed").red()
                        }
                    );
                }
            }

            // Env code is published under refs/h5i/env/code/* (above); it must
            // never live under refs/heads/ on the remote, where a host like
            // GitHub would render it as a branch. Delete any such head refs (only
            // present if an older h5i pushed them). Best-effort, idempotent.
            if let Ok(out) = git_out(&["ls-remote", "--heads", &remote, "refs/heads/h5i/env/*"]) {
                let stale: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|l| l.split_whitespace().nth(1).map(str::to_owned))
                    .collect();
                if !stale.is_empty() {
                    print!(
                        "  {} removing {} env branch(es) from {}'s head namespace … ",
                        style("⌫").dim(),
                        stale.len(),
                        style(&remote).yellow()
                    );
                    std::io::stdout().flush()?;
                    let mut args: Vec<String> =
                        vec!["push".into(), remote.clone(), "--delete".into()];
                    args.extend(stale);
                    let ok = std::process::Command::new("git")
                        .args(&args)
                        .current_dir(&workdir)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    println!(
                        "{}",
                        if ok {
                            style("ok").green()
                        } else {
                            style("skipped").dim()
                        }
                    );
                }
            }

            // Bind to the original variable name so the existing "Tip:" footer
            // (gated on notes_status.success()) keeps working unchanged.
            let notes_status_success = notes_pushed;

            if notes_status_success {
                println!(
                    "\n{} To receive these refs on another machine:\n\
                    \n    git fetch {} refs/h5i/notes:refs/h5i/notes\
                    \n    git fetch {} refs/h5i/memory:refs/h5i/memory\
                    \n    git fetch {} 'refs/h5i/context/*:refs/h5i/context/*'\
                    \n    git fetch {} refs/h5i/msg:refs/h5i/msg\
                    \n\n  Or add fetch refspecs to .git/config (see README §9) so {} picks them up automatically.",
                    style("Tip:").bold(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style(&remote).yellow(),
                    style("git pull").bold()
                );
            }
        }
    Ok(())
}
