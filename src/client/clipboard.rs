//! Copy generated commands to the system clipboard.
//!
//! One mechanism per platform, selected by session type — no fallback
//! chains. Failures are loud; the caller decides whether they are fatal.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Copy `text` to the system clipboard.
pub fn copy(text: &str) -> Result<()> {
    let (mut command, name) = platform_command()?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "clipboard copy failed: cannot run `{name}` \
                 (disable with `clipboard = false` under [preferences])"
            )
        })?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&encode(text))
        .with_context(|| format!("clipboard copy failed: cannot write to `{name}`"))?;

    let status = child
        .wait()
        .with_context(|| format!("clipboard copy failed: `{name}` did not run"))?;
    anyhow::ensure!(
        status.success(),
        "clipboard copy failed: `{name}` exited with {status} \
         (disable with `clipboard = false` under [preferences])"
    );
    Ok(())
}

/// The clipboard helper for this platform.
#[cfg(target_os = "macos")]
fn platform_command() -> Result<(Command, &'static str)> {
    Ok((Command::new("pbcopy"), "pbcopy"))
}

/// Wayland and X11 sessions each have exactly one supported helper; which
/// one applies is decided by the session environment, not by trial.
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_command() -> Result<(Command, &'static str)> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Ok((Command::new("wl-copy"), "wl-copy"))
    } else if std::env::var_os("DISPLAY").is_some() {
        let mut command = Command::new("xclip");
        command.args(["-selection", "clipboard"]);
        Ok((command, "xclip"))
    } else {
        Err(anyhow::anyhow!(
            "clipboard copy failed: no Wayland or X11 session \
             (disable with `clipboard = false` under [preferences])"
        ))
    }
}

#[cfg(windows)]
fn platform_command() -> Result<(Command, &'static str)> {
    Ok((Command::new("clip"), "clip"))
}

/// Bytes to feed the helper. On Windows, `clip` interprets plain stdin in
/// the OEM codepage; a UTF-16LE BOM makes it copy Unicode losslessly.
#[cfg(not(windows))]
fn encode(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

#[cfg(windows)]
fn encode(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}
