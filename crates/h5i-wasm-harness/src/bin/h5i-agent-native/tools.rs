//! Real-FS tool executors for the `h5i-agent` host. Semantics and output wording are
//! the reference the wasm hosts must match. An equivalence check diffs full
//! transcript dumps of the same scripted session run natively and as wasm.
//!
//! Path confinement: reject (not rewrite) absolute paths and any traversal that
//! escapes the workspace root.

use std::path::{Path, PathBuf};

use h5i_wasm_harness::json::Value;

fn normalize(workdir: &Path, raw: &str) -> Result<(PathBuf, String), String> {
    if raw.starts_with('/') {
        return Err(format!("absolute paths are not allowed: {}", raw));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("path escapes the workspace: {}", raw));
                }
            }
            p => parts.push(p),
        }
    }
    let rel = parts.join("/");
    Ok((workdir.join(&rel), rel))
}

fn str_arg(args: &Value, key: &str) -> String {
    args.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn run(workdir: &Path, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "read_file" => {
            let (abs, rel) = normalize(workdir, &str_arg(args, "path"))?;
            std::fs::read_to_string(&abs).map_err(|_| format!("no such file: {}", rel))
        }
        "write_file" => {
            let (abs, rel) = normalize(workdir, &str_arg(args, "path"))?;
            if rel.is_empty() {
                return Err("empty path".to_string());
            }
            let content = str_arg(args, "content");
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&abs, &content).map_err(|e| e.to_string())?;
            Ok(format!("wrote {} bytes to {}", content.len(), rel))
        }
        "list_dir" => {
            let (abs, rel) = normalize(workdir, &str_arg(args, "path"))?;
            let entries =
                std::fs::read_dir(&abs).map_err(|_| format!("no such directory: {}", rel))?;
            let mut names: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let mut name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    name.push('/');
                }
                names.push(name);
            }
            names.sort();
            Ok(names.join("\n"))
        }
        "bash" => {
            // Only reachable when --bash declared the tool. cwd is the workdir,
            // which is a working directory, not a jail. Real confinement is the
            // h5i sandbox's job.
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(str_arg(args, "command"))
                .current_dir(workdir)
                .output()
                .map_err(|e| format!("failed to run bash: {}", e))?;
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            if text.trim().is_empty() {
                text = format!("(no output, exit {})", out.status.code().unwrap_or(-1));
            }
            if out.status.success() { Ok(text) } else { Err(text) }
        }
        _ => Err(format!("host has no executor for tool: {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("h5i-agent-tools-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn confinement_rejects_escapes() {
        let dir = tmpdir();
        let args = h5i_wasm_harness::json::parse(r#"{"path": "../oops", "content": "x"}"#).unwrap();
        let err = run(&dir, "write_file", &args).unwrap_err();
        assert!(err.contains("escapes the workspace"));
        let args = h5i_wasm_harness::json::parse(r#"{"path": "/etc/passwd"}"#).unwrap();
        let err = run(&dir, "read_file", &args).unwrap_err();
        assert!(err.contains("absolute paths"));
        // Dotted traversal that stays inside is fine.
        let args =
            h5i_wasm_harness::json::parse(r#"{"path": "a/../b.txt", "content": "x"}"#).unwrap();
        assert_eq!(run(&dir, "write_file", &args).unwrap(), "wrote 1 bytes to b.txt");
    }

    #[test]
    fn write_read_list_roundtrip() {
        let dir = tmpdir();
        let args =
            h5i_wasm_harness::json::parse(r#"{"path": "sub/f.txt", "content": "hello"}"#).unwrap();
        run(&dir, "write_file", &args).unwrap();
        let args = h5i_wasm_harness::json::parse(r#"{"path": "sub/f.txt"}"#).unwrap();
        assert_eq!(run(&dir, "read_file", &args).unwrap(), "hello");
        let args = h5i_wasm_harness::json::parse(r#"{"path": ""}"#).unwrap();
        let listing = run(&dir, "list_dir", &args).unwrap();
        assert!(listing.contains("sub/"));
    }
}
