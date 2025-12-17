//! System context gathering for better command generation.
//!
//! Collects information about the user's environment to help the LLM
//! generate more appropriate commands.

use crate::protocol::Context;
use anyhow::Result;
use std::path::PathBuf;

/// Gather system context for the LLM.
pub fn gather_context() -> Result<Context> {
    Ok(Context {
        cwd: get_cwd(),
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

/// Get OS information from uname.
fn get_os_info() -> String {
    // Try to get uname info
    #[cfg(unix)]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("uname").arg("-a").output() {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
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
                if let Some(pretty_name) = line.strip_prefix("PRETTY_NAME=") {
                    // Remove surrounding quotes
                    let name = pretty_name.trim_matches('"');
                    return Some(name.to_string());
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Some(format!("macOS {}", version));
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
        // Should return something, either from env or fallback
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_get_os_info() {
        let os = get_os_info();
        assert!(!os.is_empty());
    }
}
