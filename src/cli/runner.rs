//! `h5i runner` — the CLI over `h5i-runner`.
//!
//! The library decides policy and moves bytes; every word the user reads lives
//! here, next to the rest of the CLI's voice. Same split as `share.rs`.
//!
//! What R13.1 built is pairing and probing. The design is ROADMAP.md R1 to
//! R13, and the sentence the rest of it hangs on: **a runner requires Linux and
//! the h5i protocol, and everything past that is an advertised capability that
//! is refused when absent, never silently weakened.**

use std::path::Path;
use std::process::Command;

use clap::Subcommand;
use console::style;

use h5i_core::ui::UI;
use h5i_runner::config::{self, RunnerRecord};
use h5i_runner::identity::{self, HostKey};
use h5i_runner::proto::Capabilities;
use h5i_runner::transport::{self, Deadlines, SshTransport};
use h5i_runner::{Client, Worker};

/// Verbs under `h5i runner`.
#[derive(Subcommand)]
pub enum RunnerCommands {
    /// Pair with a Linux machine so boxes can run on it.
    ///
    /// Pairing generates a keypair used for this runner and nothing else,
    /// pins the machine's SSH host key, and installs a forced command that
    /// lets the key do exactly one thing: speak h5i's protocol on stdin and
    /// stdout. No port is opened on the runner and no daemon is left behind.
    ///
    /// The runner needs `h5i` installed and an account you can already reach
    /// over SSH. It does NOT need a container runtime: what it can do is
    /// reported by `h5i runner probe`, and a box asking for something the
    /// runner does not have is refused rather than quietly given something
    /// weaker.
    Pair {
        /// A short name for this machine, used in commands and output. It is a
        /// label: the runner's identity is its host key, so renaming or
        /// re-pointing a name never moves a box to different hardware.
        name: String,

        /// Where it is: `user@host`, or just `host` to use your SSH default.
        #[arg(value_name = "[USER@]HOST")]
        destination: String,

        /// SSH port, when it is not 22.
        #[arg(long)]
        port: Option<u16>,

        /// Refuse to pair unless the host key matches this fingerprint.
        ///
        /// Pairing otherwise trusts the key the machine presents the first
        /// time, exactly like your first `ssh` to a new host. Read the real
        /// one on the machine itself with
        /// `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` and pass it here
        /// to close that window.
        #[arg(long, value_name = "SHA256:…")]
        fingerprint: Option<String>,

        /// Print the `authorized_keys` line instead of installing it.
        ///
        /// For a runner where you add keys by another route, or where you want
        /// to read the line before it goes in.
        #[arg(long)]
        print_only: bool,

        /// Path to `h5i` on the runner, when it is not on the PATH of the
        /// account you are pairing with.
        #[arg(long, value_name = "PATH")]
        worker_path: Option<String>,
    },

    /// Ask a runner what it can do right now.
    ///
    /// Memory, disk, which isolation tiers actually work, whether it has a
    /// container runtime, whether box state survives a reboot, and whether it
    /// reaches the internet itself. Everything here can change between one
    /// call and the next, which is why it is a question rather than something
    /// pairing recorded once.
    Probe {
        name: String,
        #[arg(long)]
        json: bool,
    },

    /// List the machines this account has paired.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },

    /// Forget a runner: its record, its key, and its pinned host key.
    ///
    /// Nothing is removed from the runner itself. The `authorized_keys` line
    /// stays there until you delete it, and the printed reminder says so.
    Unpair { name: String },

    /// Serve the runner protocol on stdin and stdout. Not for humans.
    ///
    /// This is what the forced command runs on the far side. It speaks a
    /// binary protocol: running it in a terminal does nothing useful.
    #[command(hide = true, name = "serve-stdio")]
    ServeStdio,
}

pub fn run(action: RunnerCommands) -> anyhow::Result<()> {
    match action {
        RunnerCommands::Pair {
            name,
            destination,
            port,
            fingerprint,
            print_only,
            worker_path,
        } => pair(&name, &destination, port, fingerprint, print_only, worker_path),
        RunnerCommands::Probe { name, json } => probe(&name, json),
        RunnerCommands::List { json } => list(json),
        RunnerCommands::Unpair { name } => unpair(&name),
        RunnerCommands::ServeStdio => serve_stdio(),
    }
}

// ─── the worker end ──────────────────────────────────────────────────────────

fn serve_stdio() -> anyhow::Result<()> {
    let mut worker = Worker::new(Worker::default_state_dir());
    // Nothing but frames may reach stdout, so a failure is reported on stderr
    // and by the exit status. The client captures both.
    match h5i_runner::serve::serve_stdio(&mut worker) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("h5i runner: {e}");
            std::process::exit(1);
        }
    }
}

// ─── pairing ─────────────────────────────────────────────────────────────────

fn pair(
    name: &str,
    destination: &str,
    port: Option<u16>,
    expect_fingerprint: Option<String>,
    print_only: bool,
    worker_path: Option<String>,
) -> anyhow::Result<()> {
    config::validated_name(name)?;
    if config::load(name).is_ok() {
        anyhow::bail!(
            "a runner named `{name}` is already paired — `h5i runner unpair {name}` first, \
             or pick another name"
        );
    }
    let (user, host) = split_destination(destination);

    UI::action(&format!("Pairing `{name}` with {destination}"));

    // 1. The host key, before anything else: it is this machine's identity and
    //    everything after it is authenticated against it.
    let key = scan_host_key(&host, port)?;
    let runner_id = key.runner_id();
    let fingerprint = key.fingerprint();

    match &expect_fingerprint {
        Some(expected) => {
            if expected.trim() != fingerprint {
                anyhow::bail!(
                    "the host key does not match the fingerprint you gave.\n  \
                     expected  {expected}\n  presented {fingerprint}\n\
                     Either this is not the machine you meant, or something is between you \
                     and it. Nothing has been written."
                );
            }
            UI::success(&format!("Host key matches the fingerprint you gave: {fingerprint}"));
        }
        None => {
            UI::info(&format!("Host key {fingerprint}"));
            UI::info(
                "Trusted on first sight, like your first ssh to a new host. Verify it on the \
                 machine itself with `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`, and \
                 pass `--fingerprint` next time to have h5i check it for you.",
            );
        }
    }

    // 2. Somewhere to keep it. Written before the key is generated so a
    //    half-finished pair leaves a directory to clean up rather than a
    //    private key in a place with no record of what it is for.
    let record = RunnerRecord {
        name: name.to_string(),
        host: host.clone(),
        user: user.clone(),
        port,
        runner_id: runner_id.clone(),
        fingerprint: fingerprint.clone(),
        worker_path: worker_path.clone().unwrap_or_default(),
        paired_at: chrono::Utc::now(),
    };
    let dir = config::save_new(&record)?;

    // Everything after this point may fail, and a partially paired runner is
    // worse than none: it holds a key that authorises something we never
    // confirmed works. So each failure removes the directory on the way out.
    let finish = || -> anyhow::Result<RunnerRecord> {
        config::write_private(
            &record.known_hosts_path()?,
            key.known_hosts_line(&host, port).as_bytes(),
        )?;

        // 3. A keypair for this runner and nothing else.
        let identity_path = record.identity_path()?;
        generate_keypair(&identity_path, name)?;
        let public_key = std::fs::read_to_string(record.public_key_path()?)?;

        // 4. Where `h5i` is over there. The forced command needs an absolute
        //    path, because a PATH on the far side is not something we control.
        let resolved_worker = match &worker_path {
            Some(p) => p.clone(),
            None => resolve_worker_path(&record)?,
        };

        // 5. The one line that makes the key useful and harmless.
        let line = transport::authorized_keys_line(Path::new(&resolved_worker), &public_key);
        if print_only {
            print_install_instructions(&record, &line);
        } else {
            install_authorized_key(&record, &line)?;
            UI::success("Installed the forced command on the runner");
        }

        let mut record = record.clone();
        record.worker_path = resolved_worker;
        config::save(&record)?;
        Ok(record)
    };

    let record = match finish() {
        Ok(r) => r,
        Err(e) => {
            let _ = config::remove(name);
            return Err(e.context(format!(
                "pairing `{name}` failed, and nothing was left behind on this machine"
            )));
        }
    };

    if print_only {
        UI::info("Once that line is in place, run `h5i runner probe` to finish.");
        return Ok(());
    }

    // 6. Prove the whole path works, over the pair key, through the forced
    //    command. Anything short of this is a guess about a machine we have
    //    just written a private key for.
    UI::action("Checking the runner answers over the pair key");
    match client_for(&record)?.probe() {
        Ok(probed) => {
            record.store_capabilities(&probed.capabilities)?;
            UI::success(&format!(
                "Paired `{name}` — h5i {} on {} {}",
                probed.ack.h5i_version, probed.ack.os, probed.ack.arch
            ));
            println!();
            print_capabilities(&record, &probed.capabilities);
            println!();
            UI::info(&format!("Runner id {}", identity::short(&record.runner_id)));
            UI::info(&format!("Keys and pin under {}", dir.display()));
        }
        Err(e) => {
            // The record stays: the key is installed over there, and deleting
            // our side would leave an authorised key with nothing to explain
            // it. Say what to do instead.
            UI::error(&format!("The runner did not answer: {e}"));
            println!();
            UI::info(&format!(
                "The pairing is recorded and the key is installed. Fix the runner and run \
                 `h5i runner probe {name}`, or `h5i runner unpair {name}` to undo this side."
            ));
            anyhow::bail!("pairing `{name}` did not complete");
        }
    }
    Ok(())
}

/// Fetch the machine's host key with `ssh-keyscan`.
fn scan_host_key(host: &str, port: Option<u16>) -> anyhow::Result<HostKey> {
    let mut cmd = Command::new("ssh-keyscan");
    cmd.arg("-T").arg("10");
    if let Some(p) = port {
        cmd.arg("-p").arg(p.to_string());
    }
    cmd.arg(host);

    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("`ssh-keyscan` is not installed — it ships with OpenSSH")
        } else {
            anyhow::anyhow!("could not run `ssh-keyscan`: {e}")
        }
    })?;

    let text = String::from_utf8_lossy(&out.stdout);
    identity::preferred_host_key(&text).map_err(|_| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::anyhow!(
            "could not read an SSH host key from {host}{} — is sshd running and reachable?{}",
            port.map(|p| format!(":{p}")).unwrap_or_default(),
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stderr.trim())
            }
        )
    })
}

/// A keypair used for this runner and nothing else.
fn generate_keypair(path: &Path, name: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // ssh-keygen refuses to overwrite non-interactively, and a leftover key
    // from a failed pair would otherwise make this look like it worked.
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("pub"));

    let out = Command::new("ssh-keygen")
        .args(["-t", "ed25519"])
        .args(["-N", ""]) // no passphrase: nothing may prompt on this path
        .arg("-f")
        .arg(path)
        .args(["-C", &format!("h5i-runner-{name}")])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("`ssh-keygen` is not installed — it ships with OpenSSH")
            } else {
                anyhow::anyhow!("could not run `ssh-keygen`: {e}")
            }
        })?;

    if !out.status.success() {
        anyhow::bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// `ssh` using the *user's* own credentials, for the two setup steps that
/// happen before the pair key exists on the far side.
///
/// The pinned `known_hosts` is already in force here: pairing authenticates the
/// machine from the first command it runs, not from the first one that matters.
fn setup_ssh(record: &RunnerRecord, remote_command: &str) -> anyhow::Result<Command> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile={}",
            record.known_hosts_path()?.display()
        ))
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("RequestTTY=no");
    if let Some(p) = record.port {
        cmd.arg("-p").arg(p.to_string());
    }
    cmd.arg(destination_of(record));
    cmd.arg(remote_command);
    Ok(cmd)
}

/// Where `h5i` is on the runner.
fn resolve_worker_path(record: &RunnerRecord) -> anyhow::Result<String> {
    let out = setup_ssh(record, "command -v h5i")?
        .output()
        .map_err(|e| anyhow::anyhow!("could not run ssh: {e}"))?;

    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && path.starts_with('/') {
        return Ok(path);
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("Permission denied") || stderr.contains("Host key verification") {
        anyhow::bail!(
            "could not log in to {} to set up pairing:\n{}\n\
             Pairing needs an account you can already reach over SSH.",
            destination_of(record),
            stderr.trim()
        );
    }
    anyhow::bail!(
        "`h5i` is not on the PATH of {} — install it on the runner, or pass \
         `--worker-path /path/to/h5i`.{}",
        destination_of(record),
        if stderr.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}", stderr.trim())
        }
    )
}

/// Append the forced-command line to the runner's `authorized_keys`.
///
/// Written by a small shell fragment rather than by editing the file from here,
/// because the file belongs to the runner: this appends, creates the directory
/// with the modes sshd insists on, and refuses to add the same key twice.
fn install_authorized_key(record: &RunnerRecord, line: &str) -> anyhow::Result<()> {
    // The line is passed on stdin rather than in argv: argv is world-readable
    // in `/proc` on an ordinary Linux box, and while a public key is not a
    // secret, a command line that varies per runner is a habit worth not
    // forming.
    let script = "\
set -eu
mkdir -p ~/.ssh
chmod 700 ~/.ssh
touch ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
line=$(cat)
key=$(printf '%s' \"$line\" | awk '{print $(NF-1)}')
if grep -qF \"$key\" ~/.ssh/authorized_keys 2>/dev/null; then
  echo 'h5i: this key is already authorised' >&2
  exit 0
fi
printf '%s\\n' \"$line\" >> ~/.ssh/authorized_keys
";

    let mut child = setup_ssh(record, script)?
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run ssh: {e}"))?;

    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(line.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!(
            "could not install the forced command on {}:\n{}",
            destination_of(record),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn print_install_instructions(record: &RunnerRecord, line: &str) {
    println!();
    UI::action(&format!(
        "Add this one line to ~/.ssh/authorized_keys on {}",
        destination_of(record)
    ));
    println!();
    println!("{}", style(line).cyan());
    println!();
    UI::info(
        "`restrict` is doing the work: with it the key cannot open a shell, forward a port, \
         forward your agent, or allocate a terminal. It can run that one command and nothing \
         else.",
    );
}

// ─── probe, list, unpair ─────────────────────────────────────────────────────

fn probe(name: &str, json: bool) -> anyhow::Result<()> {
    let record = config::load(name)?;
    let probed = client_for(&record)?.probe().map_err(|e| {
        anyhow::anyhow!("could not reach runner `{name}` ({}): {e}", destination_of(&record))
    })?;
    record.store_capabilities(&probed.capabilities)?;

    if json {
        let out = serde_json::json!({
            "name": record.name,
            "runner_id": record.runner_id,
            "fingerprint": record.fingerprint,
            "host": record.host,
            "user": record.user,
            "port": record.port,
            "h5i_version": probed.ack.h5i_version,
            "protocol": probed.ack.protocol,
            "capabilities": probed.capabilities,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    UI::success(&format!(
        "`{name}` — h5i {} on {} {}, protocol {}",
        probed.ack.h5i_version, probed.ack.os, probed.ack.arch, probed.ack.protocol
    ));
    println!();
    print_capabilities(&record, &probed.capabilities);
    Ok(())
}

fn list(json: bool) -> anyhow::Result<()> {
    let runners = config::list()?;

    if json {
        let rows: Vec<_> = runners
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "runner_id": r.runner_id,
                    "fingerprint": r.fingerprint,
                    "host": r.host,
                    "user": r.user,
                    "port": r.port,
                    "paired_at": r.paired_at,
                    // Labelled a cache everywhere it appears: it is what the
                    // last probe saw, not what is true now.
                    "cached_capabilities": r.cached_capabilities(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if runners.is_empty() {
        UI::info("No runners paired. `h5i runner pair <name> <user@host>` adds one.");
        return Ok(());
    }

    for r in &runners {
        let caps = r.cached_capabilities();
        let summary = match &caps {
            Some(c) => format!(
                "{} {}, tiers: {}",
                c.arch,
                human_mb(c.memory_mb),
                if c.isolation.is_empty() {
                    "none".to_string()
                } else {
                    c.isolation.join(", ")
                }
            ),
            None => "not probed yet".to_string(),
        };
        println!(
            "  {}  {}",
            style(&r.name).bold(),
            style(destination_of(r)).dim()
        );
        println!(
            "      {}  {}",
            style(&summary).dim(),
            style(format!("id {}", identity::short(&r.runner_id))).dim()
        );
    }
    println!();
    UI::info("What each can do is from the last probe. `h5i runner probe <name>` asks again.");
    Ok(())
}

fn unpair(name: &str) -> anyhow::Result<()> {
    let record = config::load(name)?;
    let dir = config::remove(name)?;
    UI::success(&format!("Forgot `{name}` — removed {}", dir.display()));
    UI::info(&format!(
        "The forced-command line is still in ~/.ssh/authorized_keys on {}. Remove it there \
         to finish: look for the line ending `h5i-runner-{name}`.",
        destination_of(&record)
    ));
    Ok(())
}

// ─── shared ──────────────────────────────────────────────────────────────────

fn client_for(record: &RunnerRecord) -> anyhow::Result<Client> {
    let transport = SshTransport {
        host: record.host.clone(),
        user: record.user.clone(),
        port: record.port,
        identity: record.identity_path()?,
        known_hosts: record.known_hosts_path()?,
        control_path: record.control_path(),
        remote_command: format!("{} runner serve-stdio", record.worker_path),
        deadlines: Deadlines::default(),
    };
    Ok(Client::new(Box::new(transport)))
}

fn destination_of(record: &RunnerRecord) -> String {
    match &record.user {
        Some(u) => format!("{u}@{}", record.host),
        None => record.host.clone(),
    }
}

/// `user@host` or `host`.
fn split_destination(destination: &str) -> (Option<String>, String) {
    match destination.split_once('@') {
        Some((user, host)) if !user.is_empty() && !host.is_empty() => {
            (Some(user.to_string()), host.to_string())
        }
        _ => (None, destination.to_string()),
    }
}

fn print_capabilities(record: &RunnerRecord, caps: &Capabilities) {
    let tiers = if caps.isolation.is_empty() {
        style("none verified".to_string()).red().to_string()
    } else {
        caps.isolation.join(", ")
    };

    println!("  {:<14}{}", style("isolation").dim(), tiers);
    println!(
        "  {:<14}{}",
        style("container").dim(),
        yes_no(caps.container)
    );
    println!("  {:<14}{}", style("memory").dim(), human_mb(caps.memory_mb));
    println!(
        "  {:<14}{} free",
        style("workspace").dim(),
        human_mb(caps.workspace_mb)
    );
    println!(
        "  {:<14}{}",
        style("boxes persist").dim(),
        yes_no(caps.persistent_boxes)
    );
    println!(
        "  {:<14}{}",
        style("own egress").dim(),
        yes_no(caps.own_egress)
    );
    println!("  {:<14}{}", style("kvm").dim(), yes_no(caps.kvm));
    println!(
        "  {:<14}{}",
        style("runner id").dim(),
        identity::short(&record.runner_id)
    );

    for note in &caps.notes {
        println!("  {} {}", style("·").dim(), style(note).dim());
    }

    // The two capabilities whose absence changes what a user can do next are
    // worth a sentence rather than a `no` in a table.
    if !caps.persistent_boxes {
        println!();
        UI::warning(
            "Box state on this runner does not survive a reboot. A reboot is an expired \
             lease: anything not exported is gone.",
        );
    }
    if !caps.own_egress {
        println!();
        UI::warning(
            "This runner has no default route, so a box on it cannot pull images or install \
             packages. Egress brokered through this machine is a later milestone.",
        );
    }
}

fn yes_no(b: bool) -> String {
    if b {
        style("yes").green().to_string()
    } else {
        style("no").dim().to_string()
    }
}

fn human_mb(mb: u64) -> String {
    if mb == 0 {
        return "unknown".into();
    }
    if mb >= 1024 {
        format!("{:.1} GiB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MiB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_splits_into_user_and_host() {
        assert_eq!(
            split_destination("h5i@pi.local"),
            (Some("h5i".into()), "pi.local".into())
        );
        assert_eq!(split_destination("pi.local"), (None, "pi.local".into()));
        // Malformed halves fall back to "all of it is the host", so ssh gets
        // the string the user typed and reports on it in its own words.
        assert_eq!(split_destination("@pi.local"), (None, "@pi.local".into()));
        assert_eq!(split_destination("h5i@"), (None, "h5i@".into()));
    }

    #[test]
    fn sizes_read_as_sizes_and_zero_reads_as_unknown() {
        // Zero is what `host` reports when it could not measure, and printing
        // "0 MiB" would state a fact nobody established.
        assert_eq!(human_mb(0), "unknown");
        assert_eq!(human_mb(512), "512 MiB");
        assert_eq!(human_mb(16384), "16.0 GiB");
    }
}
