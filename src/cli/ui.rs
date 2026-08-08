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
    let mut command = browser_command(url);
    let opener = command.get_program().to_string_lossy().into_owned();
    if let Err(e) = command
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

fn browser_command(url: &str) -> std::process::Command {
    browser_command_for(url, std::env::consts::OS)
}

fn browser_command_for(url: &str, target_os: &str) -> std::process::Command {
    let (program, args) = match target_os {
        "macos" => ("open", vec![url]),
        "windows" => ("cmd", vec!["/C", "start", "", url]),
        _ => ("xdg-open", vec![url]),
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    command
}

#[cfg(test)]
mod tests {
    use super::browser_command_for;

    #[test]
    fn browser_command_uses_platform_opener() {
        let url = "http://127.0.0.1:8080/?token=test";
        let cases = [
            ("macos", "open", vec![url]),
            ("windows", "cmd", vec!["/C", "start", "", url]),
            ("linux", "xdg-open", vec![url]),
        ];

        for (target_os, expected_program, expected_args) in cases {
            let command = browser_command_for(url, target_os);
            let program = command.get_program().to_string_lossy();
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect();

            assert_eq!(program, expected_program);
            assert_eq!(args, expected_args);
        }
    }
}
