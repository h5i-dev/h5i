//! `h5i pull` — CLI handler (migrated from main.rs).
use crate::*;

pub fn run(remote: String, force: bool) -> anyhow::Result<()> {
    {
            let workdir = std::env::current_dir()?;

            println!(
                "{} {} from {}",
                STEP,
                style("Pulling all h5i refs").cyan().bold(),
                style(&remote).yellow()
            );

            use std::io::Write as _;

            // Helper: run `git <args>` in the working dir, capturing output.
            let git = |args: &[&str]| -> std::io::Result<std::process::Output> {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&workdir)
                    .output()
            };

            // Helper: resolve a ref to its full SHA, or None if it doesn't exist.
            let resolve_ref = |refname: &str| -> Option<String> {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "--quiet", refname])
                    .current_dir(&workdir)
                    .output()
                    .ok()?;
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    None
                }
            };

            // Helper: is `ancestor` an ancestor of `descendant`?
            let is_ancestor = |ancestor: &str, descendant: &str| -> bool {
                std::process::Command::new("git")
                    .args(["merge-base", "--is-ancestor", ancestor, descendant])
                    .current_dir(&workdir)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };

            // Sync one h5i ref from the remote, choosing the safest action that
            // preserves local data:
            //
            //   missing on remote → skip
            //   no local copy     → install (fast install)
            //   identical         → up to date
            //   local ⊑ remote    → fast-forward
            //   remote ⊑ local    → keep local (we're ahead)
            //   diverged          → notes: union-merge; others: keep unless --force
            //
            // We always fetch into a per-call temp ref under refs/h5i/_incoming/
            // first so the remote's value can never overwrite the live local ref
            // implicitly — every ref update goes through `git update-ref` here.
            // The temp ref is deleted at the end of each call.
            //
            // Returns true iff the live local ref was changed by this call.
            let sync_one = |refname: &str| -> anyhow::Result<bool> {
                print!("  {} {} … ", style("→").dim(), style(refname).yellow());
                std::io::stdout().flush()?;

                let basename = refname.rsplit('/').next().unwrap_or("ref");
                let incoming = format!("refs/h5i/_incoming/{}", basename);

                // Always force-fetch into the temp ref. The temp ref is
                // private to this call, so this can never destroy user data;
                // it just guarantees we get the remote's latest into a known
                // local name we can compare against.
                let fetch_refspec = format!("+{}:{}", refname, incoming);
                let fetch = git(&["fetch", "--no-write-fetch-head", &remote, &fetch_refspec])?;

                if !fetch.status.success() {
                    let stderr = String::from_utf8_lossy(&fetch.stderr);
                    let missing = stderr.contains("couldn't find remote ref")
                        || stderr.contains("does not exist");
                    if missing {
                        println!(
                            "{} ({})",
                            style("skipped").yellow(),
                            style("not present on remote").dim()
                        );
                    } else {
                        println!("{}", style("failed").red());
                        eprint!("{}", stderr);
                    }
                    return Ok(false);
                }

                let local = resolve_ref(refname);
                let incoming_oid = match resolve_ref(&incoming) {
                    Some(oid) => oid,
                    None => {
                        println!("{}", style("failed").red());
                        eprintln!(
                            "internal: fetched {} but could not resolve {}",
                            refname, incoming
                        );
                        return Ok(false);
                    }
                };

                // Outcome decided per-branch; helper closures keep the match
                // arms readable without repeating the update-ref + report code.
                let install = |label: &str| -> anyhow::Result<bool> {
                    let st = git(&["update-ref", refname, &incoming_oid])?;
                    if !st.status.success() {
                        println!("{}", style("failed").red());
                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                        Ok(false)
                    } else {
                        println!("{} ({})", style("ok").green(), style(label).dim());
                        Ok(true)
                    }
                };

                let updated = match local.as_deref() {
                    None => install("new")?,
                    Some(l) if l == incoming_oid => {
                        println!("{} ({})", style("ok").green(), style("up to date").dim());
                        false
                    }
                    Some(l) if is_ancestor(l, &incoming_oid) => install("fast-forward")?,
                    Some(l) if is_ancestor(&incoming_oid, l) => {
                        println!(
                            "{} ({})",
                            style("ok").green(),
                            style("local ahead — kept").dim()
                        );
                        false
                    }
                    Some(local_oid_str) => {
                        // Diverged. For `refs/h5i/notes` we can union-merge
                        // safely because each tree entry is keyed by a
                        // content-addressed code-commit OID, so disjoint
                        // annotations never overlap. Other refs (memory /
                        // context / ast) are linear chains where merging
                        // would require domain-specific knowledge — for
                        // those we keep local unless --force.
                        //
                        // We can't use `git notes merge` directly: it
                        // refuses to operate on refs outside `refs/notes/*`.
                        // Instead we drive the merge ourselves via git2,
                        // build the merged commit, and update the ref to
                        // point at it.
                        if refname == "refs/h5i/notes" {
                            let g2 = git2::Repository::open(&workdir)
                                .map_err(|e| anyhow::anyhow!("open git2 repo: {e}"))?;
                            let local_git2 = git2::Oid::from_str(local_oid_str)
                                .map_err(|e| anyhow::anyhow!("parse local oid: {e}"))?;
                            let incoming_git2 = git2::Oid::from_str(&incoming_oid)
                                .map_err(|e| anyhow::anyhow!("parse incoming oid: {e}"))?;
                            let merge_result =
                                union_merge_notes_commits(&g2, local_git2, incoming_git2);
                            match merge_result {
                                Ok(new_oid) => {
                                    let new_oid_str = new_oid.to_string();
                                    let st =
                                        git(&["update-ref", refname, &new_oid_str, local_oid_str])?;
                                    if st.status.success() {
                                        println!(
                                            "{} ({})",
                                            style("ok").green(),
                                            style("merged (union)").dim()
                                        );
                                        true
                                    } else {
                                        println!("{}", style("failed").red());
                                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                                        false
                                    }
                                }
                                Err(e) => {
                                    println!("{}", style("failed").red());
                                    eprintln!("union-merge of notes refs failed: {e}");
                                    false
                                }
                            }
                        } else if refname == msg::MSG_REF {
                            // The message log is strictly append-only, so a
                            // divergence is just two disjoint sets of appended
                            // messages. Union-merge them by id (analogous to
                            // notes) so no message is ever lost on pull.
                            let g2 = git2::Repository::open(&workdir)
                                .map_err(|e| anyhow::anyhow!("open git2 repo: {e}"))?;
                            let local_git2 = git2::Oid::from_str(local_oid_str)
                                .map_err(|e| anyhow::anyhow!("parse local oid: {e}"))?;
                            let incoming_git2 = git2::Oid::from_str(&incoming_oid)
                                .map_err(|e| anyhow::anyhow!("parse incoming oid: {e}"))?;
                            match msg::union_merge_commits(&g2, local_git2, incoming_git2) {
                                Ok(new_oid) => {
                                    let new_oid_str = new_oid.to_string();
                                    let st =
                                        git(&["update-ref", refname, &new_oid_str, local_oid_str])?;
                                    if st.status.success() {
                                        println!(
                                            "{} ({})",
                                            style("ok").green(),
                                            style("merged (union)").dim()
                                        );
                                        true
                                    } else {
                                        println!("{}", style("failed").red());
                                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                                        false
                                    }
                                }
                                Err(e) => {
                                    println!("{}", style("failed").red());
                                    eprintln!("union-merge of msg refs failed: {e}");
                                    false
                                }
                            }
                        } else if refname == h5i_core::objects::OBJECTS_REF {
                            // The object-manifest log is append-only too: union
                            // the two disjoint sets of pointers so a captured
                            // summary is never lost when two clones diverge.
                            let g2 = git2::Repository::open(&workdir)
                                .map_err(|e| anyhow::anyhow!("open git2 repo: {e}"))?;
                            let local_git2 = git2::Oid::from_str(local_oid_str)
                                .map_err(|e| anyhow::anyhow!("parse local oid: {e}"))?;
                            let incoming_git2 = git2::Oid::from_str(&incoming_oid)
                                .map_err(|e| anyhow::anyhow!("parse incoming oid: {e}"))?;
                            match h5i_core::objects::union_merge_commits(
                                &g2,
                                local_git2,
                                incoming_git2,
                            ) {
                                Ok(new_oid) => {
                                    let new_oid_str = new_oid.to_string();
                                    let st =
                                        git(&["update-ref", refname, &new_oid_str, local_oid_str])?;
                                    if st.status.success() {
                                        println!(
                                            "{} ({})",
                                            style("ok").green(),
                                            style("merged (union)").dim()
                                        );
                                        true
                                    } else {
                                        println!("{}", style("failed").red());
                                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                                        false
                                    }
                                }
                                Err(e) => {
                                    println!("{}", style("failed").red());
                                    eprintln!("union-merge of objects refs failed: {e}");
                                    false
                                }
                            }
                        } else if refname == h5i_core::env::ENV_REF {
                            // The env event log is append-only: union the two
                            // sides (dedup on env_id|ts|event) so no lifecycle
                            // event is lost when two clones diverge.
                            let g2 = git2::Repository::open(&workdir)
                                .map_err(|e| anyhow::anyhow!("open git2 repo: {e}"))?;
                            let local_git2 = git2::Oid::from_str(local_oid_str)
                                .map_err(|e| anyhow::anyhow!("parse local oid: {e}"))?;
                            let incoming_git2 = git2::Oid::from_str(&incoming_oid)
                                .map_err(|e| anyhow::anyhow!("parse incoming oid: {e}"))?;
                            match h5i_core::env::union_merge_commits(&g2, local_git2, incoming_git2)
                            {
                                Ok(new_oid) => {
                                    let new_oid_str = new_oid.to_string();
                                    let st =
                                        git(&["update-ref", refname, &new_oid_str, local_oid_str])?;
                                    if st.status.success() {
                                        println!(
                                            "{} ({})",
                                            style("ok").green(),
                                            style("merged (union)").dim()
                                        );
                                        true
                                    } else {
                                        println!("{}", style("failed").red());
                                        eprint!("{}", String::from_utf8_lossy(&st.stderr));
                                        false
                                    }
                                }
                                Err(e) => {
                                    println!("{}", style("failed").red());
                                    eprintln!("union-merge of env refs failed: {e}");
                                    false
                                }
                            }
                        } else if force {
                            install("forced over divergent local")?
                        } else {
                            println!(
                                "{} ({})",
                                style("kept local").yellow(),
                                style("diverged — pass --force to overwrite").dim()
                            );
                            false
                        }
                    }
                };

                // Always clean up the temp ref. We ignore errors here because
                // (a) it's best-effort housekeeping and (b) `update-ref -d`
                // returns success even if the ref is already gone on most git
                // versions, but we don't want a flaky cleanup to mask the
                // primary outcome.
                let _ = git(&["update-ref", "-d", &incoming]);

                Ok(updated)
            };

            let notes_changed = sync_one("refs/h5i/notes")?;
            sync_one(memory::MEMORY_REF)?;

            // Context refs: per-branch. Fetch the whole namespace into a temp
            // tree first, then sync each branch through the same safe-merge
            // logic. Legacy single ref (`refs/h5i/context`) is also tried for
            // backward compat with pre-redesign remotes.
            {
                let fetch = git(&[
                    "fetch",
                    "--no-write-fetch-head",
                    &remote,
                    "+refs/h5i/context/*:refs/h5i/_incoming/context/*",
                ])?;
                if !fetch.status.success() {
                    let stderr = String::from_utf8_lossy(&fetch.stderr);
                    if !stderr.contains("couldn't find remote ref")
                        && !stderr.contains("does not exist")
                    {
                        eprint!("{}", stderr);
                    }
                }
                // Enumerate fetched per-branch refs and sync each.
                if let Ok(out) = std::process::Command::new("git")
                    .args([
                        "for-each-ref",
                        "--format=%(refname)",
                        "refs/h5i/_incoming/context/",
                    ])
                    .current_dir(&workdir)
                    .output()
                {
                    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
                    let mut branch_names: Vec<String> = listing
                        .lines()
                        .filter_map(|l| {
                            l.strip_prefix("refs/h5i/_incoming/context/")
                                .map(str::to_owned)
                        })
                        .collect();
                    branch_names.sort();
                    for branch in &branch_names {
                        let live = format!("refs/h5i/context/{branch}");
                        // sync_one re-fetches into refs/h5i/_incoming/<basename>
                        // and uses the safe compare-and-install dance. Reusing
                        // it keeps semantics identical to other h5i refs.
                        let _ = sync_one(&live);
                    }
                    // Clean up the namespace temp refs.
                    for branch in &branch_names {
                        let incoming = format!("refs/h5i/_incoming/context/{branch}");
                        let _ = git(&["update-ref", "-d", &incoming]);
                    }
                }
                // Also try the legacy single ref (older remotes that pre-date
                // the per-branch redesign).
                let _ = sync_one("refs/h5i/context");
            }

            sync_one(msg::MSG_REF)?;
            sync_one(h5i_core::objects::OBJECTS_REF)?;
            // Shareable env state (manifests + policies + events). The
            // union-merge dispatch in `sync_one` reconciles divergence.
            sync_one(h5i_core::env::ENV_REF)?;
            // Fetch the env CODE branches so pulled environments can be
            // reviewed/applied from their committed state. They arrive from the
            // hidden `refs/h5i/env/code/*` namespace into local `refs/heads/h5i/env/*`.
            // Fast-forward only; a diverged local env branch is kept (the
            // reviewer's own work).
            print!(
                "  {} {} … ",
                style("→").dim(),
                style("refs/h5i/env/code/*").yellow()
            );
            std::io::stdout().flush()?;
            let env_fetch = git(&[
                "fetch",
                "--no-write-fetch-head",
                &remote,
                ENV_CODE_FETCH_REFSPEC,
            ])?;
            let env_ok = env_fetch.status.success();
            println!(
                "{}",
                if env_ok {
                    style("ok").green()
                } else {
                    style("skipped").dim()
                }
            );
            // Materialize any newly-arrived env manifests/policies onto disk so
            // `h5i env list/status/diff/apply` see them immediately.
            if let Ok(repo) = git2::Repository::open(&workdir) {
                if let Ok(h5i_root) = h5i_core::storage::h5i_root_for_repo(&repo) {
                    match h5i_core::env::materialize_from_ref(&repo, &h5i_root) {
                        Ok(n) if n > 0 => println!(
                            "  {} materialized {n} shared environment(s)",
                            style("✓").green()
                        ),
                        _ => {}
                    }
                }
            }

            if notes_changed {
                println!(
                    "\n{} Inspect what arrived with:\n\
                    \n    {}\
                    \n    {}\
                    \n    {}",
                    style("Tip:").bold(),
                    style("h5i log").bold(),
                    style("h5i notes show").bold(),
                    style("h5i memory log").bold(),
                );
            }
        }
    Ok(())
}
