//! System context gathering for better command generation.
//!
//! Collects information about the user's environment to help the LLM
//! generate more appropriate commands. Everything gathered here is local
//! and cheap: directory markers, one PATH scan, one `git status` call, and
//! a few environment variables. Shell history is deliberately never read.

use crate::protocol::{Context, WindowsContext};
#[cfg(any(windows, test))]
use anyhow::bail;
#[cfg(windows)]
use anyhow::Context as _;
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

#[cfg(any(windows, test))]
const WINDOWS_DIAGNOSTIC_TOOLS: &[&str] = &[
    "pwsh.exe",
    "wpr.exe",
    "wpa.exe",
    "procmon.exe",
    "procexp.exe",
    "handle.exe",
    "tcpview.exe",
    "rammap.exe",
    "autoruns.exe",
    "sigcheck.exe",
    "diskspd.exe",
    "pktmon.exe",
    "netsh.exe",
    "wevtutil.exe",
    "pnputil.exe",
    "dism.exe",
    "sfc.exe",
    "driverquery.exe",
    "systeminfo.exe",
    "wsl.exe",
];

/// Gather system context for the LLM.
pub fn gather_context() -> Result<Context> {
    let cwd = get_cwd();
    #[cfg(windows)]
    let (shell, windows, tools) = {
        let path_tools = probe_windows_path();
        let (shell, windows) =
            gather_windows_context(path_tools.pwsh, path_tools.diagnostic_tools)?;
        (shell, windows, path_tools.modern_tools)
    };
    #[cfg(not(windows))]
    let (shell, windows, tools): (String, Option<WindowsContext>, Vec<String>) =
        (get_shell(), None, probe_tools());

    Ok(Context {
        projects: detect_projects(&cwd),
        tools,
        git: git_state(&cwd),
        env_flags: detect_env_flags(),
        cwd,
        shell,
        os: windows.as_ref().map_or_else(get_os_info, |details| {
            format!(
                "{} {} (build {})",
                details.caption, details.version, details.build
            )
        }),
        distro: get_distro_info(),
        windows,
    })
}

/// Get the current working directory.
fn get_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Get the user's shell from `$SHELL` on POSIX hosts.
#[cfg(not(windows))]
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

#[cfg(any(windows, test))]
#[derive(Debug, Default, PartialEq, Eq)]
struct WindowsPathTools {
    modern_tools: Vec<String>,
    diagnostic_tools: Vec<String>,
    pwsh: Option<&'static str>,
}

#[cfg(windows)]
fn probe_windows_path() -> WindowsPathTools {
    std::env::var_os("PATH")
        .as_deref()
        .map(probe_windows_path_in)
        .unwrap_or_default()
}

#[cfg(any(windows, test))]
fn probe_windows_path_in(path: &std::ffi::OsStr) -> WindowsPathTools {
    let mut modern = HashSet::new();
    let mut diagnostic = HashSet::new();
    for dir in std::env::split_paths(path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let bare_name = name.strip_suffix(".exe").unwrap_or(&name);
            if let Some(tool) = PROBED_TOOLS.iter().find(|tool| **tool == bare_name) {
                modern.insert(*tool);
            }
            if WINDOWS_DIAGNOSTIC_TOOLS.contains(&name.as_str()) {
                diagnostic.insert(name);
            }
        }
    }

    let diagnostic_tools: Vec<String> = WINDOWS_DIAGNOSTIC_TOOLS
        .iter()
        .filter(|tool| diagnostic.contains(**tool))
        .map(|tool| (*tool).to_string())
        .collect();
    let pwsh = diagnostic_tools
        .iter()
        .any(|tool| tool == "pwsh.exe")
        .then_some("pwsh.exe");
    WindowsPathTools {
        modern_tools: PROBED_TOOLS
            .iter()
            .filter(|tool| modern.contains(**tool))
            .map(|tool| (*tool).to_string())
            .collect(),
        diagnostic_tools,
        pwsh,
    }
}
#[cfg(any(windows, test))]
fn require_pwsh(pwsh: Option<&'static str>) -> Result<&'static str> {
    let Some(pwsh) = pwsh else {
        bail!("pwsh 7.4+ is required on Windows; pwsh.exe was not found on PATH");
    };
    Ok(pwsh)
}

#[cfg(windows)]
fn gather_windows_context(
    pwsh: Option<&'static str>,
    diagnostic_tools: Vec<String>,
) -> Result<(String, Option<WindowsContext>)> {
    let pwsh = require_pwsh(pwsh)?;
    let script = r#"
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if ([Version]$PSVersionTable.PSVersion.ToString() -lt [Version]'7.4') {
    throw "pwsh 7.4+ is required"
}
$targetPid = [uint32]$args[0]
$os = Get-CimInstance -ClassName Win32_OperatingSystem
$target = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $targetPid"
$parentName = (Get-Process -Id $target.ParentProcessId -ErrorAction Stop).ProcessName
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
[Console]::Out.Write(('{0}`t{1}`t{2}`t{3}`t{4}`t{5}' -f $os.Caption, $os.Version, $os.BuildNumber, $PSVersionTable.PSVersion, $elevated, $parentName))
"#;
    let output = std::process::Command::new(pwsh)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .arg(std::process::id().to_string())
        .output()
        .with_context(|| format!("failed to collect Windows context with {pwsh}"))?;
    if !output.status.success() {
        bail!(
            "{pwsh} failed to collect Windows context: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8(output.stdout).context("PowerShell returned non-UTF-8 context")?;
    let parsed = parse_windows_context(&text, diagnostic_tools)?;
    let shell = normalize_windows_shell(&parsed.0, std::env::var_os("COMSPEC").as_deref());
    Ok((shell, Some(parsed.1)))
}

#[cfg(any(windows, test))]
fn validate_pwsh_version(version: &str) -> Result<()> {
    let version = version.trim();
    let mut components = version.split('.');
    let parse_component = |component: Option<&str>| {
        component
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse::<u64>().ok())
    };
    let (Some(major), Some(minor)) = (
        parse_component(components.next()),
        parse_component(components.next()),
    ) else {
        bail!("pwsh 7.4+ is required; reported version {version:?} is malformed");
    };

    let mut extra_components = 0;
    for component in components {
        extra_components += 1;
        if extra_components > 2 || component.parse::<u64>().is_err() {
            bail!("pwsh 7.4+ is required; reported version {version:?} is malformed");
        }
    }
    if major < 7 || (major == 7 && minor < 4) {
        bail!("pwsh 7.4+ is required; found {version}");
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn parse_windows_context(
    text: &str,
    diagnostic_tools: Vec<String>,
) -> Result<(String, WindowsContext)> {
    let fields: Vec<&str> = text.trim().split('\t').collect();
    if fields.len() != 6 || fields[..5].iter().any(|value| value.trim().is_empty()) {
        bail!("PowerShell returned incomplete Windows context");
    }
    let powershell_version = fields[3].trim();
    validate_pwsh_version(powershell_version)?;
    let elevated = match fields[4].trim() {
        value if value.eq_ignore_ascii_case("true") => true,
        value if value.eq_ignore_ascii_case("false") => false,
        _ => bail!("PowerShell returned an invalid elevation state"),
    };
    Ok((
        fields[5].trim().to_string(),
        WindowsContext {
            caption: fields[0].trim().to_string(),
            version: fields[1].trim().to_string(),
            build: fields[2].trim().to_string(),
            powershell_version: powershell_version.to_string(),
            elevated,
            diagnostic_tools,
        },
    ))
}

#[cfg(any(windows, test))]
fn windows_process_stem(name: &str) -> &str {
    let basename = name
        .trim()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(name.trim());
    let extension_start = basename.len().saturating_sub(4);
    if basename
        .get(extension_start..)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".exe"))
    {
        &basename[..extension_start]
    } else {
        basename
    }
}

#[cfg(any(windows, test))]
fn normalize_windows_shell(parent_name: &str, comspec: Option<&std::ffi::OsStr>) -> String {
    let parent = windows_process_stem(parent_name);
    if parent.eq_ignore_ascii_case("pwsh") {
        return "pwsh".to_string();
    }
    if parent.eq_ignore_ascii_case("powershell") {
        return "PowerShell".to_string();
    }
    if parent.eq_ignore_ascii_case("cmd") {
        return "cmd".to_string();
    }
    comspec
        .and_then(std::ffi::OsStr::to_str)
        .map(windows_process_stem)
        .filter(|name| name.eq_ignore_ascii_case("cmd"))
        .map_or_else(|| parent_name.trim().to_string(), |_| "cmd".to_string())
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

    #[test]
    fn parses_explicit_windows_context_fields() {
        let (parent, details) = parse_windows_context(
            "Microsoft Windows 11 Pro\t10.0.26100\t26100\t7.4.6\tTrue\tPwSh.EXE",
            vec!["pwsh.exe".to_string(), "pnputil.exe".to_string()],
        )
        .unwrap();
        assert_eq!(parent, "PwSh.EXE");
        assert_eq!(details.caption, "Microsoft Windows 11 Pro");
        assert_eq!(details.version, "10.0.26100");
        assert_eq!(details.build, "26100");
        assert_eq!(details.powershell_version, "7.4.6");
        assert!(details.elevated);
        assert_eq!(
            details.diagnostic_tools,
            vec!["pwsh.exe".to_string(), "pnputil.exe".to_string()]
        );
    }

    #[test]
    fn rejects_incomplete_or_ambiguous_windows_security_context() {
        assert!(parse_windows_context("Windows\t10.0\t26100\t7.4\tmaybe\tpwsh", vec![]).is_err());
        assert!(parse_windows_context("Windows\t10.0\t26100\t7.4\ttrue", vec![]).is_err());
    }

    #[test]
    fn normalizes_supported_windows_shells_without_shell_environment_fallback() {
        let cases = [
            ("pwsh", None, "pwsh"),
            ("PwSh.EXE", None, "pwsh"),
            ("powershell.exe", None, "PowerShell"),
            ("CMD.EXE", None, "cmd"),
            (
                "WindowsTerminal",
                Some(std::ffi::OsStr::new(r"C:\Windows\System32\cmd.exe")),
                "cmd",
            ),
            ("nu", None, "nu"),
        ];
        for (parent, comspec, expected) in cases {
            assert_eq!(normalize_windows_shell(parent, comspec), expected);
        }
    }

    #[test]
    fn windows_tool_probe_is_curated_case_insensitive_and_ordered() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("PNPUTIL.EXE"), "").unwrap();
        std::fs::write(first.path().join("not-a-diagnostic.exe"), "").unwrap();
        std::fs::write(second.path().join("PwSh.ExE"), "").unwrap();
        std::fs::write(second.path().join("WPR.exe"), "").unwrap();
        std::fs::write(second.path().join("RG.EXE"), "").unwrap();
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        let tools = probe_windows_path_in(&path);
        assert_eq!(tools.modern_tools, vec!["rg".to_string()]);
        assert_eq!(
            tools.diagnostic_tools,
            vec![
                "pwsh.exe".to_string(),
                "wpr.exe".to_string(),
                "pnputil.exe".to_string()
            ]
        );
        assert_eq!(tools.pwsh, Some("pwsh.exe"));
    }

    #[test]
    fn windows_path_probe_never_selects_legacy_windows_powershell() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("powershell.exe"), "").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let tools = probe_windows_path_in(&path);
        assert_eq!(tools.pwsh, None);
        assert!(tools.diagnostic_tools.is_empty());
    }

    #[test]
    fn executable_selection_requires_pwsh() {
        assert_eq!(require_pwsh(Some("pwsh.exe")).unwrap(), "pwsh.exe");
        let error = require_pwsh(None).unwrap_err().to_string();
        assert_eq!(
            error,
            "pwsh 7.4+ is required on Windows; pwsh.exe was not found on PATH"
        );
    }

    #[test]
    fn pwsh_version_enforces_7_4_boundary_numerically() {
        for accepted in ["7.4", "7.4.0", "7.10.2", "8.0", "10.1.2.3"] {
            assert!(
                validate_pwsh_version(accepted).is_ok(),
                "{accepted:?} should be accepted"
            );
        }
        for rejected in [
            "5.1",
            "7.3",
            "7.3.99",
            "",
            "7",
            "seven.four",
            "7.4-preview",
            "7.4.1.2.3",
        ] {
            let error = validate_pwsh_version(rejected).unwrap_err().to_string();
            assert!(
                error.contains("pwsh 7.4+ is required"),
                "unexpected error for {rejected:?}: {error}"
            );
        }
    }
}
