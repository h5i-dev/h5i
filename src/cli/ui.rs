//! `h5i ui`: CLI handler for the box console.
//!
//! The command is deliberately thin: bind, print the one URL that works, and
//! block. Everything else is [`h5i_core::server`], including the token, which
//! is why the URL is printed rather than constructed here.

use console::style;

pub fn run(port: u16, open: bool) -> anyhow::Result<()> {
    // Discover from the cwd so the console shows the fleet of the repository
    // the human is standing in, exactly like every other verb.
    let repo = super::discover_repo("h5i ui")?;
    let repo_path = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("h5i ui requires a non-bare repository"))?
        .to_path_buf();

    let console_srv = h5i_core::server::Console::bind(repo_path, port)?;
    let url = console_srv.url()?;

    println!(
        "{} {}",
        style("✓").green().bold(),
        style("h5i box console").green().bold()
    );
    println!("  {}", style(&url).underlined().blue());
    println!(
        "  {}",
        style("read-only · loopback only · the token above is this session's").dim()
    );
    println!("  {}\n", style("Press Ctrl-C to stop").dim());

    if open {
        // Not `url`: `launch_browser` puts its argument on another process's
        // command line, and `/proc/<pid>/cmdline` is world-readable. The
        // handoff URL carries a single-use token instead, spent by the page
        // load it exists for.
        launch_browser(&console_srv.handoff_url()?);
    }
    console_srv.serve()?;
    Ok(())
}

/// Hand the URL to the desktop's browser. Best-effort and non-fatal: the URL
/// is already on screen, so a host without a handler loses nothing.
fn launch_browser(url: &str) {
    let (opener, mut command) = browser_command_for(url, std::env::consts::OS);
    let child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            eprintln!(
                "{} could not run {opener}: {e} — open the URL above yourself",
                style("warning:").yellow()
            );
            return;
        }
    };

    // Spawning is not the failure that matters. `start` runs behind a `cmd`
    // that is always present, and `xdg-open` reports a missing handler in its
    // exit status rather than at spawn, so the status is the only signal that
    // the browser did not come up. Waiting happens off-thread because some
    // `xdg-open` backends block until the browser itself exits, and the console
    // has to start serving now; the thread doubles as the child's reaper.
    std::thread::spawn(move || match child.wait() {
        Ok(status) if !status.success() => eprintln!(
            "{} {opener} failed ({status}) — open the URL above yourself",
            style("warning:").yellow()
        ),
        _ => {}
    });
}

/// The opener for `target_os`, paired with the name to blame in a warning,
/// which is not always the program, since Windows reaches the browser through
/// `cmd`. Split from the host so every arm is reachable from one build.
fn browser_command_for(url: &str, target_os: &str) -> (&'static str, std::process::Command) {
    // `start` reads a lone quoted argument as the new window's title, so the
    // empty title slot is what keeps it from swallowing the URL.
    let (label, program, args) = match target_os {
        "macos" => ("open", "open", vec![url]),
        "windows" => ("cmd /C start", "cmd", vec!["/C", "start", "", url]),
        _ => ("xdg-open", "xdg-open", vec![url]),
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    (label, command)
}

#[cfg(test)]
mod tests {
    use super::browser_command_for;

    /// Every target h5i releases for, plus one it does not, so the fallback arm
    /// is exercised rather than assumed.
    const TARGETS: [&str; 4] = ["macos", "windows", "linux", "freebsd"];

    fn argv(url: &str, target_os: &str) -> (String, Vec<String>) {
        let (_, command) = browser_command_for(url, target_os);
        (
            command.get_program().to_string_lossy().into_owned(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        )
    }

    /// The token-bearing URL reaches the opener as one argv element, verbatim.
    /// The Windows arm goes through `cmd`, which re-parses its command line, so
    /// this is the guard that a URL is never assembled into a shell string.
    #[test]
    fn the_url_reaches_every_opener_as_a_single_untouched_argument() {
        let url = "http://127.0.0.1:8080/?token=deadbeef&next=%2F";
        for target_os in TARGETS {
            let (program, args) = argv(url, target_os);
            assert!(!program.is_empty(), "{target_os}: no opener");
            assert_eq!(
                args.iter().filter(|arg| arg.as_str() == url).count(),
                1,
                "{target_os}: url is not exactly one argument: {args:?}"
            );
            assert_eq!(
                args.last().map(String::as_str),
                Some(url),
                "{target_os}: url is not the last argument: {args:?}"
            );
        }
    }

    /// A host h5i was never built for still gets the freedesktop opener rather
    /// than no opener at all.
    #[test]
    fn an_unrecognized_host_falls_back_to_the_freedesktop_opener() {
        assert_eq!(argv("http://127.0.0.1:8080/", "freebsd").0, "xdg-open");
        assert_eq!(argv("http://127.0.0.1:8080/", "").0, "xdg-open");
    }

    /// `start` treats a lone quoted argument as the window title. Without the
    /// empty title slot it would consume the URL and open a console instead of
    /// a browser, so the slot is the invariant worth pinning.
    #[test]
    fn the_windows_opener_keeps_starts_empty_title_slot() {
        let (_, args) = argv("http://127.0.0.1:8080/", "windows");
        let head: Vec<&str> = args.iter().take(3).map(String::as_str).collect();
        assert_eq!(head, ["/C", "start", ""], "{args:?}");
    }

    /// The warning has to name something the user can act on. On Windows the
    /// program is `cmd`, which says nothing about opening a browser.
    #[test]
    fn the_failure_label_names_the_opener_not_its_shell() {
        for target_os in TARGETS {
            let (label, command) = browser_command_for("http://127.0.0.1:8080/", target_os);
            assert!(
                label.contains(&*command.get_program().to_string_lossy()),
                "{target_os}: label {label:?} does not name the program"
            );
        }
        assert_eq!(
            browser_command_for("http://127.0.0.1:8080/", "windows").0,
            "cmd /C start"
        );
    }
}
