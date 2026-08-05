//! `h5i ui` — CLI handler for the box console.
//!
//! The command is deliberately thin: bind, print the one URL that works, and
//! block. Everything else is [`h5i_core::server`], including the token — which
//! is why the URL is printed rather than constructed here.

use console::style;

pub fn run(port: u16, open: bool) -> anyhow::Result<()> {
    // Discover from the cwd so the console shows the fleet of the repository
    // the human is standing in, exactly like every other verb.
    let repo = git2::Repository::discover(".")?;
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
        launch_browser(&url);
    }
    console_srv.serve()?;
    Ok(())
}

/// Hand the URL to the desktop's browser. Best-effort and non-fatal: the URL
/// is already on screen, so a host without a handler loses nothing.
fn launch_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!(
            "{} could not run {opener}: {e} — open the URL above yourself",
            style("warning:").yellow()
        );
    }
}
