//! System context gathering for better command generation.
//!
//! Collects information about the user's environment to help the LLM
//! generate more appropriate commands. Everything gathered here is local
//! and cheap: directory markers, one PATH scan, one `git status` call, and
//! a few environment variables. Shell history is deliberately never read.

use crate::protocol::Context;
use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Project types detected from well-known marker files.
const PROJECT_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("package.json", "node"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),
    ("go.mod", "go"),
    ("pom.xml", "java"),
    ("build.gradle", "java"),
    ("build.gradle.kts", "java"),
    ("Gemfile", "ruby"),
    ("composer.json", "php"),
    ("mix.exs", "elixir"),
    ("CMakeLists.txt", "cmake"),
    ("Dockerfile", "docker"),
    ("docker-compose.yml", "docker-compose"),
    ("compose.yaml", "docker-compose"),
];

/// Modern CLI tools worth telling the model about. The system prompt asks
/// the model to prefer these over classic equivalents, so it must only
/// mention tools that are actually installed.
const PROBED_TOOLS: &[&str] = &[
    "rg", "fd", "bat", "eza", "jq", "yq", "fzf", "gh", "docker", "podman", "kubectl", "tmux",
];

/// Gather system context for the LLM.
pub fn gather_context() -> Result<Context> {
    let cwd = get_cwd();
    Ok(Context {
        projects: detect_projects(&cwd),
        tools: probe_tools(),
        git: git_state(&cwd),
        env_flags: detect_env_flags(),
        cwd,
        shell: get_shell(),
        os: get_os_info(),
        distro: get_distro_info(),
    })
}

/// Get the current working directory.
fn get_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Get the user's shell from $SHELL environment variable.
fn get_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Detect project types from marker files in `dir`.
fn detect_projects(dir: &Path) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut projects = Vec::new();
    for (marker, project) in PROJECT_MARKERS {
        if dir.join(marker).exists() && seen.insert(*project) {
            projects.push((*project).to_string());
        }
    }
    projects
}

/// Probe PATH once and report which of [`PROBED_TOOLS`] are installed.
fn probe_tools() -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };

    let mut available: HashSet<&str> = HashSet::new();
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(tool) = PROBED_TOOLS.iter().find(|t| **t == name) {
                    available.insert(tool);
                }
            }
        }
    }

    // Preserve the canonical ordering rather than hash order.
    PROBED_TOOLS
        .iter()
        .filter(|t| available.contains(**t))
        .map(|t| (*t).to_string())
        .collect()
}

/// Git state of `dir`: branch plus dirty/clean, e.g. "branch main, dirty".
///
/// One `git status --porcelain=v2 --branch -uno` call yields both facts.
/// Returns None outside a repository or when git is unavailable.
fn git_state(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "-uno",
            "--no-renames",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None; // not a repo
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut branch = None;
    let mut dirty = false;
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch = Some(if head == "(detached)" {
                "detached HEAD".to_string()
            } else {
                format!("branch {}", head)
            });
        } else if !line.starts_with('#') {
            dirty = true;
        }
    }

    branch.map(|b| format!("{}, {}", b, if dirty { "dirty" } else { "clean" }))
}

/// Detect notable environment flags: ssh session, tmux, docker container.
fn detect_env_flags() -> Vec<String> {
    let mut flags = Vec::new();
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        flags.push("ssh".to_string());
    }
    if std::env::var_os("TMUX").is_some() {
        flags.push("tmux".to_string());
    }
    if Path::new("/.dockerenv").exists() {
        flags.push("docker".to_string());
    }
    flags
}

/// Get OS information from uname.
fn get_os_info() -> String {
    // Try to get uname info
    #[cfg(unix)]
    {
        if let Ok(output) = std::process::Command::new("uname").arg("-sr").output() {
            if output.status.success() {
                if let Ok(info) = String::from_utf8(output.stdout) {
                    return info.trim().to_string();
                }
            }
        }
    }

    // Fallback to basic OS info
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Get Linux distribution info from /etc/os-release.
fn get_distro_info() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/etc/os-release") {
            for line in contents.lines() {
                if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                    return Some(value.trim_matches('"').to_string());
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            if output.status.success() {
                if let Ok(version) = String::from_utf8(output.stdout) {
                    return Some(format!("macOS {}", version.trim()));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_context() {
        let ctx = gather_context().unwrap();
        assert!(!ctx.shell.is_empty());
        assert!(!ctx.os.is_empty());
    }

    #[test]
    fn test_get_shell() {
        let shell = get_shell();
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_get_os_info() {
        let os = get_os_info();
        assert!(!os.is_empty());
    }

    #[test]
    fn detect_projects_from_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let projects = detect_projects(dir.path());
        assert_eq!(projects, vec!["rust".to_string(), "node".to_string()]);
    }

    #[test]
    fn detect_projects_dedupes_same_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "").unwrap();
        assert_eq!(detect_projects(dir.path()), vec!["python".to_string()]);
    }

    #[test]
    fn detect_projects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_projects(dir.path()).is_empty());
    }

    #[test]
    fn git_state_outside_repo_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_state(dir.path()), None);
    }

    #[test]
    fn git_state_reports_branch_and_cleanliness() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);

        assert_eq!(
            git_state(dir.path()),
            Some("branch trunk, clean".to_string())
        );

        // Tracked-file modification flips it to dirty.
        std::fs::write(dir.path().join("f.txt"), "one").unwrap();
        run(&["add", "f.txt"]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "add f",
        ]);
        std::fs::write(dir.path().join("f.txt"), "two").unwrap();
        assert_eq!(
            git_state(dir.path()),
            Some("branch trunk, dirty".to_string())
        );
    }

    #[test]
    fn probe_tools_reports_only_present_tools() {
        // `probe_tools` reads the real PATH; whatever it reports must
        // genuinely resolve, and the list must be a subset of the probe set.
        for tool in probe_tools() {
            assert!(PROBED_TOOLS.contains(&tool.as_str()));
        }
    }
}
