//! Advisory safety analysis for generated commands.
//!
//! incant never executes anything — the user always reviews the command in
//! their shell buffer. But the whole premise of the tool is that the user may
//! not fully understand the command they asked for, so the daemon flags
//! patterns that are destructive or commonly regretted before the user
//! presses Enter.
//!
//! This is a heuristic advisory layer, not a sandbox. It reduces the chance
//! of running `rm -rf ~` by accident; it makes no guarantee about adversarial
//! or novel commands, and it must never be treated as a security boundary.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;

/// How risky a generated command looks.
///
/// Ordering matters: `Safe < Caution < Destructive`. An assessment's overall
/// level is the maximum across findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// No known dangerous pattern matched.
    Safe,
    /// Discards data or state in a way that is hard to reverse; worth a
    /// second look before running.
    Caution,
    /// Can irreversibly destroy data, the system, or lock the user out.
    Destructive,
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskLevel::Safe => write!(f, "safe"),
            RiskLevel::Caution => write!(f, "caution"),
            RiskLevel::Destructive => write!(f, "destructive"),
        }
    }
}

/// A single matched rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Stable rule identifier (useful in logs and tests).
    pub rule: String,
    /// Risk level this rule assigns.
    pub level: RiskLevel,
    /// Human-readable explanation shown to the user.
    pub reason: String,
}

/// The daemon's verdict on a generated command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    /// Highest risk level across all findings.
    pub level: RiskLevel,
    /// Every rule that matched, most severe first.
    pub findings: Vec<Finding>,
}

impl Assessment {
    /// True when nothing dangerous matched.
    pub fn is_safe(&self) -> bool {
        self.level == RiskLevel::Safe
    }
}

/// A detection rule. `all` patterns must every one match; a match is
/// suppressed when `unless` matches (the regex crate has no lookaround, so
/// exceptions are expressed structurally instead).
struct Rule {
    id: &'static str,
    level: RiskLevel,
    reason: &'static str,
    all: Vec<Regex>,
    unless: Option<Regex>,
    segment_scoped: bool,
}

impl Rule {
    fn new(
        id: &'static str,
        level: RiskLevel,
        reason: &'static str,
        all: &[&str],
        unless: Option<&str>,
    ) -> Self {
        let segment_scoped = id.starts_with("powershell-") || id.starts_with("windows-");
        Self {
            id,
            level,
            reason,
            all: all
                .iter()
                .map(|p| Regex::new(p).expect("static safety pattern must compile"))
                .collect(),
            unless: unless.map(|p| Regex::new(p).expect("static safety pattern must compile")),
            segment_scoped,
        }
    }

    fn matches(&self, command: &str) -> bool {
        if !self.segment_scoped {
            return self.all.iter().all(|re| re.is_match(command))
                && self.unless.as_ref().is_none_or(|re| !re.is_match(command));
        }

        any_powershell_segment(command, |segment| {
            let Some(primary) = self.all.first() else {
                return false;
            };
            if !self.all[1..].iter().all(|re| re.is_match(segment)) {
                return false;
            }

            primary.find_iter(segment).any(|primary_match| {
                let invocation = powershell_invocation_scope(segment, primary_match.start());
                self.unless
                    .as_ref()
                    .is_none_or(|re| !re.is_match(&mask_powershell_non_code(invocation)))
            })
        })
    }
}

/// Apply a rule to each PowerShell command segment. Separators inside quoted
/// strings and escaped separators remain part of the current command.
fn any_powershell_segment(command: &str, mut predicate: impl FnMut(&str) -> bool) -> bool {
    let bytes = command.as_bytes();
    let mut start = 0;
    let mut index = 0;
    let mut quote = None;

    while index < bytes.len() {
        match quote {
            Some(b'\'') => {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    quote = None;
                }
                index += 1;
            }
            Some(b'"') => {
                if bytes[index] == b'`' && index + 1 < bytes.len() {
                    index += 2;
                } else {
                    if bytes[index] == b'"' {
                        quote = None;
                    }
                    index += 1;
                }
            }
            Some(_) => unreachable!("only PowerShell quote bytes are stored"),
            None => {
                if bytes[index] == b'`' && index + 1 < bytes.len() {
                    index += 2;
                    continue;
                }
                if matches!(bytes[index], b'\'' | b'"') {
                    quote = Some(bytes[index]);
                    index += 1;
                    continue;
                }

                let separator_len = match bytes[index] {
                    b'\r' => usize::from(bytes.get(index + 1) == Some(&b'\n')) + 1,
                    b'\n' | b';' => 1,
                    b'&' if bytes.get(index + 1) == Some(&b'&') => 2,
                    b'|' => usize::from(bytes.get(index + 1) == Some(&b'|')) + 1,
                    _ => 0,
                };
                if separator_len == 0 {
                    index += 1;
                    continue;
                }
                if predicate(command[start..index].trim()) {
                    return true;
                }
                index += separator_len;
                start = index;
            }
        }
    }

    predicate(command[start..].trim())
}

/// Limit an exception such as `-WhatIf` to the invocation containing the
/// matched cmdlet. A nested `$(...)` or script block cannot borrow an outer
/// command's common parameters.
fn powershell_invocation_scope(segment: &str, match_start: usize) -> &str {
    let tail = &segment[match_start..];
    let Some((offset, opener)) = tail
        .char_indices()
        .find(|(_, character)| !character.is_ascii_whitespace())
    else {
        return tail;
    };
    let closer = match opener {
        '(' => b')',
        '{' => b'}',
        _ => return tail,
    };

    let bytes = tail.as_bytes();
    let opener = opener as u8;
    let mut depth = 0;
    let mut quote = None;
    let mut index = offset;
    while index < bytes.len() {
        match quote {
            Some(b'\'') => {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    quote = None;
                }
            }
            Some(b'"') => {
                if bytes[index] == b'`' && index + 1 < bytes.len() {
                    index += 2;
                    continue;
                }
                if bytes[index] == b'"' {
                    quote = None;
                }
            }
            Some(_) => unreachable!("only PowerShell quote bytes are stored"),
            None if bytes[index] == b'`' && index + 1 < bytes.len() => {
                index += 2;
                continue;
            }
            None if matches!(bytes[index], b'\'' | b'"') => quote = Some(bytes[index]),
            None if bytes[index] == opener => depth += 1,
            None if bytes[index] == closer => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..index];
                }
            }
            None => {}
        }
        index += 1;
    }
    tail
}

/// Mask quoted strings and comments before looking for common parameters.
/// Their text is data, not a parameter on the matched invocation.
fn mask_powershell_non_code(command: &str) -> String {
    let mut masked = command.as_bytes().to_vec();
    let mut quote = None;
    let mut index = 0;
    while index < masked.len() {
        match quote {
            Some(b'\'') => {
                if masked[index] == b'\'' {
                    if masked.get(index + 1) == Some(&b'\'') {
                        masked[index] = b' ';
                        masked[index + 1] = b' ';
                        index += 2;
                        continue;
                    }
                    quote = None;
                } else {
                    masked[index] = b' ';
                }
            }
            Some(b'"') => {
                if masked[index] == b'`' && index + 1 < masked.len() {
                    masked[index] = b' ';
                    masked[index + 1] = b' ';
                    index += 2;
                    continue;
                }
                if masked[index] == b'"' {
                    quote = None;
                } else {
                    masked[index] = b' ';
                }
            }
            Some(_) => unreachable!("only PowerShell quote bytes are stored"),
            None if masked[index] == b'#' => {
                masked[index..].fill(b' ');
                break;
            }
            None if matches!(masked[index], b'\'' | b'"') => quote = Some(masked[index]),
            None => {}
        }
        index += 1;
    }
    String::from_utf8(masked).expect("masking ASCII bytes preserves UTF-8")
}

// Building blocks reused across rm/chmod rules.
//
// "Broad target" = an argument that expands to the filesystem root, the home
// directory, or everything in the current directory. The argument must END at
// the match (whitespace/EOL/separator), so `/tmp/foo` or `~/old` never count.
const RM_RECURSIVE: &str = r"\brm\s[^|;&]*(-[a-zA-Z]*[rR]|--recursive\b)";
const RM_FORCE: &str = r"\brm\s[^|;&]*(-[a-zA-Z]*f|--force\b)";
const RM_BROAD_TARGET: &str = r#"\brm\s[^|;&]*\s(/\*?|~/?|"?\$HOME"?/?|\*|\.\.|\.)\s*($|;|&|\|)"#;
const CHMOD_RECURSIVE_WORLD: &str = r"\bchmod\s[^|;&]*-[a-zA-Z]*R[^|;&]*\b(777|a\+rwx)\b";
const CHMOD_BROAD_TARGET: &str = r#"\bchmod\s[^|;&]*\s(/|~/?|"?\$HOME"?/?)\s*($|;|&|\|)"#;

// PowerShell common parameters are case-insensitive. `-WhatIf` suppresses only
// cmdlet rules that honor it; appending it to a native executable is not a
// safety mechanism.
const POWERSHELL_WHAT_IF: &str = r"(?i)(?:^|\s)-whatif(?:\s|$|:\s*\$true\b)";

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // ── Destructive ────────────────────────────────────────────────
        Rule::new(
            "rm-recursive-force-broad",
            RiskLevel::Destructive,
            "recursively force-deletes the filesystem root, home directory, or everything in the current directory",
            &[RM_RECURSIVE, RM_FORCE, RM_BROAD_TARGET],
            None,
        ),
        Rule::new(
            "rm-no-preserve-root",
            RiskLevel::Destructive,
            "disables the only guardrail preventing deletion of /",
            &[r"--no-preserve-root\b"],
            None,
        ),
        Rule::new(
            "dd-to-block-device",
            RiskLevel::Destructive,
            "writes raw bytes directly over a disk device",
            &[r"\bdd\s[^|;&]*\bof=/dev/\w"],
            None,
        ),
        Rule::new(
            "format-block-device",
            RiskLevel::Destructive,
            "formats or repartitions a disk, destroying its contents",
            &[r"\b(mkfs(\.\w+)?|wipefs|sgdisk|fdisk|parted)\b[^|;&]*/dev/\w"],
            None,
        ),
        Rule::new(
            "redirect-to-block-device",
            RiskLevel::Destructive,
            "redirects output directly onto a disk device",
            &[r">\s*/dev/(sd|hd|nvme|mmcblk|disk)\w*"],
            None,
        ),
        Rule::new(
            "overwrite-auth-files",
            RiskLevel::Destructive,
            "writes directly to system authentication files (passwd/shadow/sudoers)",
            &[r">>?\s*/etc/(passwd|shadow|sudoers)\b"],
            None,
        ),
        Rule::new(
            "fork-bomb",
            RiskLevel::Destructive,
            "fork bomb: spawns processes until the system is unusable",
            &[r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;?\s*:"],
            None,
        ),
        Rule::new(
            "crontab-remove-all",
            RiskLevel::Destructive,
            "deletes the entire crontab without confirmation (-r)",
            &[r"\bcrontab\s+(-\w+\s+)*-r\b"],
            None,
        ),
        Rule::new(
            "mv-to-dev-null",
            RiskLevel::Destructive,
            "moving a file into /dev/null discards it permanently",
            &[r"\bmv\s[^|;&]*\s/dev/null\s*($|;|&|\|)"],
            None,
        ),
        Rule::new(
            "kill-init",
            RiskLevel::Destructive,
            "kills PID 1, which brings the whole system down",
            &[r"\bkill\s+(-9|-KILL|-SIGKILL)\s+1\s*($|;|&|\|)"],
            None,
        ),
        Rule::new(
            "chmod-world-writable-broad",
            RiskLevel::Destructive,
            "recursively makes the filesystem root or home directory world-writable",
            &[CHMOD_RECURSIVE_WORLD, CHMOD_BROAD_TARGET],
            None,
        ),
        // ── PowerShell destructive actions ─────────────────────────────
        Rule::new(
            "powershell-registry-change",
            RiskLevel::Destructive,
            "changes or removes Windows registry keys or values",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:remove-item|ri|rm|del|erase|rd|rmdir|remove-itemproperty|rp|clear-item|cli|clear-itemproperty|clp|set-item|si|set-itemproperty|sp|new-item|ni|new-itemproperty|rename-item|rni|move-item|mi|copy-item|cpi)\b['"]?[^|;\r\n]*(?:registry::|hk(?:lm|cu|cr|u|cc):)"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-registry-native-change",
            RiskLevel::Destructive,
            "changes or removes Windows registry keys or values",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*)?reg(?:\.exe)?\s+(?:add|delete|import|restore|load|unload)\b"],
            None,
        ),
        Rule::new(
            "powershell-service-change",
            RiskLevel::Destructive,
            "changes, stops, restarts, or removes a Windows service",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:stop-service|spsv|restart-service|remove-service|set-service)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-scheduled-task-change",
            RiskLevel::Destructive,
            "registers, removes, or changes a Windows scheduled task",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:register-scheduledtask|unregister-scheduledtask|set-scheduledtask|enable-scheduledtask|disable-scheduledtask)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-schtasks-change",
            RiskLevel::Destructive,
            "creates, deletes, or changes a Windows scheduled task",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*)?schtasks(?:\.exe)?\b[^|;\r\n]*/(?:create|delete|change)\b"],
            None,
        ),
        Rule::new(
            "windows-service-native-change",
            RiskLevel::Destructive,
            "changes, stops, or removes a Windows service",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*)?sc\.exe\s+(?:stop|delete|config|failure|failureflag)\b"],
            None,
        ),
        Rule::new(
            "powershell-device-driver-change",
            RiskLevel::Destructive,
            "changes a Windows device or installed driver",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:disable-pnpdevice|enable-pnpdevice|remove-pnpdevice)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-pnputil-change",
            RiskLevel::Destructive,
            "adds, deletes, disables, removes, or restarts a Windows driver or device",
            &[r"(?i)\bpnputil(?:\.exe)?\b[^|;\r\n]*/(?:add-driver|delete-driver|disable-device|enable-device|remove-device|restart-device)\b"],
            None,
        ),
        Rule::new(
            "powershell-storage-change",
            RiskLevel::Destructive,
            "initializes, formats, clears, resizes, or repartitions storage",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:clear-disk|initialize-disk|format-volume|remove-partition|resize-partition|set-disk)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-firewall-change",
            RiskLevel::Destructive,
            "changes or removes Windows Firewall policy",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:(?:new|set|remove|disable)-netfirewall(?:rule|profile))\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-firewall-native-change",
            RiskLevel::Destructive,
            "changes or resets Windows Firewall policy",
            &[r"(?i)\bnetsh(?:\.exe)?\s+advfirewall\b[^|;\r\n]*\b(?:reset|set|add|delete)\b"],
            None,
        ),
        Rule::new(
            "powershell-network-change",
            RiskLevel::Destructive,
            "changes Windows network adapters, addresses, routes, or DNS configuration",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:disable-netadapter|restart-netadapter|new-netipaddress|remove-netipaddress|set-netipinterface|set-dnsclientserveraddress|new-netroute|remove-netroute|set-netroute)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-netsh-network-change",
            RiskLevel::Destructive,
            "changes Windows network interface configuration",
            &[r"(?i)\bnetsh(?:\.exe)?\s+interface\b[^|;\r\n]*\b(?:set|add|delete)\b"],
            None,
        ),
        Rule::new(
            "powershell-event-log-clear",
            RiskLevel::Destructive,
            "clears a Windows event log",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?clear-eventlog\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-event-log-native-clear",
            RiskLevel::Destructive,
            "clears a Windows event log",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*)?wevtutil(?:\.exe)?\s+(?:cl|clear-log)\b"],
            None,
        ),
        Rule::new(
            "windows-bcd-change",
            RiskLevel::Destructive,
            "changes the Windows boot configuration database",
            &[r"(?i)\bbcdedit(?:\.exe)?\b[^|;\r\n]*/(?:set|delete|deletevalue|create|createstore|import|copy|bootsequence|default|displayorder|timeout|toolsdisplayorder)\b"],
            None,
        ),
        Rule::new(
            "powershell-defender-change",
            RiskLevel::Destructive,
            "changes Microsoft Defender configuration",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:set-mppreference|add-mppreference|remove-mppreference)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-execution-policy-change",
            RiskLevel::Destructive,
            "changes or bypasses PowerShell execution policy",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?set-executionpolicy\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-execution-policy-bypass",
            RiskLevel::Destructive,
            "starts PowerShell with a bypassed or unrestricted execution policy",
            &[r"(?i)\b(?:powershell|pwsh)(?:\.exe)?\b[^|;\r\n]*(?:-executionpolicy|-ep)\s+(?:bypass|unrestricted)\b"],
            None,
        ),
        // ── Caution ────────────────────────────────────────────────────
        Rule::new(
            "rm-recursive-force",
            RiskLevel::Caution,
            "force-deletes recursively with no prompt; double-check the target path",
            &[RM_RECURSIVE, RM_FORCE],
            Some(RM_BROAD_TARGET),
        ),
        Rule::new(
            "chmod-world-writable",
            RiskLevel::Caution,
            "recursively makes files world-writable (777); this is rarely what you want",
            &[CHMOD_RECURSIVE_WORLD],
            Some(CHMOD_BROAD_TARGET),
        ),
        Rule::new(
            "pipe-download-to-shell",
            RiskLevel::Caution,
            "executes a remotely downloaded script without review",
            &[r"\b(curl|wget|fetch)\b[^|;&]*\|\s*(sudo\s+)?(env\s+\S+\s+)?(ba|z|da|k)?sh\b"],
            None,
        ),
        Rule::new(
            "dd-write",
            RiskLevel::Caution,
            "dd overwrites its output destination byte-for-byte",
            &[r"\bdd\s[^|;&]*\bof="],
            Some(r"\bof=/dev/\w"),
        ),
        Rule::new(
            "git-push-force",
            RiskLevel::Caution,
            "force-push rewrites remote history for everyone",
            &[r"\bgit\s+push\s[^|;&]*(--force\b|-f\b)"],
            Some(r"--force-with-lease\b"),
        ),
        Rule::new(
            "git-reset-hard",
            RiskLevel::Caution,
            "discards all uncommitted changes in the working tree",
            &[r"\bgit\s+reset\s+[^|;&]*--hard\b"],
            None,
        ),
        Rule::new(
            "git-clean-force",
            RiskLevel::Caution,
            "permanently deletes untracked files",
            &[r"\bgit\s+clean\s[^|;&]*-[a-zA-Z]*f"],
            None,
        ),
        Rule::new(
            "sql-drop",
            RiskLevel::Caution,
            "drops or truncates database objects",
            &[r"(?i)\b(drop\s+(table|database|schema|index)|truncate\s+table)\b"],
            None,
        ),
        Rule::new(
            "shred-or-truncate",
            RiskLevel::Caution,
            "irrecoverably destroys file contents",
            &[r"\b(shred|truncate\s+(-\w+\s+)*-s\s*0)\b"],
            None,
        ),
        Rule::new(
            "find-delete",
            RiskLevel::Caution,
            "find with -delete/-exec rm removes every match; run it without the delete action first",
            &[r"\bfind\s[^|;&]*(-delete\b|-exec\s+rm\b)"],
            None,
        ),
        Rule::new(
            "firewall-flush",
            RiskLevel::Caution,
            "flushes firewall rules; on a remote host this can lock you out",
            &[r"\biptables\s+(-\w+\s+)*(-F|--flush)\b|\bnft\s+flush\s+ruleset\b"],
            None,
        ),
        Rule::new(
            "system-power",
            RiskLevel::Caution,
            "shuts down or reboots the machine",
            &[r"(^|\s|;|&&\s*)(sudo\s+)?(shutdown|reboot|halt|poweroff)(\s|$|;|&)"],
            None,
        ),
        Rule::new(
            "powershell-remove-item-recursive",
            RiskLevel::Caution,
            "recursively removes PowerShell items; double-check the target path",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:remove-item|ri|rm|del|erase|rd|rmdir)\b['"]?[^|;\r\n]*(?:-recurse|-r)\b"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-force",
            RiskLevel::Caution,
            "uses PowerShell -Force to suppress a command's normal guardrails",
            &[r"(?i)(?:^|\s)-force(?:\s|$|[:=])"],
            None,
        ),
        Rule::new(
            "powershell-confirm-false",
            RiskLevel::Caution,
            "explicitly disables PowerShell confirmation prompts",
            &[r"(?i)(?:^|\s)-confirm\s*:\s*\$false\b"],
            None,
        ),
        Rule::new(
            "powershell-invoke-expression",
            RiskLevel::Caution,
            "dynamically evaluates a string as PowerShell code",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:invoke-expression|iex)\b['"]?"#],
            None,
        ),
        Rule::new(
            "powershell-encoded-command",
            RiskLevel::Caution,
            "runs an encoded PowerShell command that is difficult to review",
            &[r"(?i)\b(?:powershell|pwsh)(?:\.exe)?\b[^|;\r\n]*-(?:encodedcommand|enc)\s+\S+"],
            None,
        ),
        Rule::new(
            "powershell-runas",
            RiskLevel::Caution,
            "requests an elevated process through Start-Process -Verb RunAs",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?start-process\b['"]?[^|;\r\n]*-verb\s+['"]?runas\b"#],
            None,
        ),
    ]
});

/// Assess a generated command against the rule table.
///
/// Every matching rule becomes a [`Finding`]; the overall level is the most
/// severe finding (or [`RiskLevel::Safe`] when nothing matches).
pub fn assess(command: &str) -> Assessment {
    let mut findings: Vec<Finding> = RULES
        .iter()
        .filter(|rule| rule.matches(command))
        .map(|rule| Finding {
            rule: rule.id.to_string(),
            level: rule.level,
            reason: rule.reason.to_string(),
        })
        .collect();

    // Most severe first, so clients can print findings in order.
    findings.sort_by_key(|f| std::cmp::Reverse(f.level));

    let level = findings.first().map(|f| f.level).unwrap_or(RiskLevel::Safe);

    Assessment { level, findings }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that `command` triggers `rule` at `level`.
    fn assert_flags(command: &str, rule: &str, level: RiskLevel) {
        let a = assess(command);
        assert!(
            a.findings
                .iter()
                .any(|f| f.rule == rule && f.level == level),
            "expected {command:?} to trigger {rule} at {level}; got {:?}",
            a.findings
        );
    }

    /// Assert that `command` is completely clean.
    fn assert_safe(command: &str) {
        let a = assess(command);
        assert!(
            a.is_safe(),
            "expected {command:?} to be safe; got {:?}",
            a.findings
        );
    }

    /// Assert that `command` does NOT trigger a specific rule (it may
    /// trigger others).
    fn assert_not_rule(command: &str, rule: &str) {
        let a = assess(command);
        assert!(
            !a.findings.iter().any(|f| f.rule == rule),
            "expected {command:?} not to trigger {rule}; got {:?}",
            a.findings
        );
    }

    // ── rm ─────────────────────────────────────────────────────────────

    #[test]
    fn rm_rf_root_is_destructive() {
        for cmd in [
            "rm -rf /",
            "sudo rm -rf /",
            "rm -fr ~",
            "rm -rf ~/",
            "rm -rf $HOME",
            "rm -rf \"$HOME\"",
            "rm -rf *",
            "rm -rf .",
            "rm -rf ..",
            "rm -rf /*",
            "rm --recursive --force /",
            "cd /tmp && rm -rf .",
        ] {
            assert_flags(cmd, "rm-recursive-force-broad", RiskLevel::Destructive);
        }
    }

    #[test]
    fn rm_rf_scoped_path_is_caution_not_destructive() {
        for cmd in [
            "rm -rf /tmp/build",
            "rm -rf ./node_modules",
            "rm -rf ~/old-project",
            "rm -rf target",
        ] {
            assert_flags(cmd, "rm-recursive-force", RiskLevel::Caution);
            assert_not_rule(cmd, "rm-recursive-force-broad");
        }
    }

    #[test]
    fn plain_rm_is_safe() {
        assert_safe("rm notes.txt");
        assert_safe("rm -i old.log");
    }

    #[test]
    fn no_preserve_root_is_destructive() {
        assert_flags(
            "rm -rf --no-preserve-root /",
            "rm-no-preserve-root",
            RiskLevel::Destructive,
        );
    }

    // ── disks ──────────────────────────────────────────────────────────

    #[test]
    fn dd_to_device_is_destructive() {
        assert_flags(
            "dd if=ubuntu.iso of=/dev/sda bs=4M",
            "dd-to-block-device",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn dd_to_file_is_caution() {
        let cmd = "dd if=/dev/urandom of=key.bin bs=32 count=1";
        assert_flags(cmd, "dd-write", RiskLevel::Caution);
        assert_not_rule(cmd, "dd-to-block-device");
    }

    #[test]
    fn mkfs_is_destructive() {
        assert_flags(
            "mkfs.ext4 /dev/sdb1",
            "format-block-device",
            RiskLevel::Destructive,
        );
        assert_flags(
            "sudo wipefs --all /dev/nvme0n1",
            "format-block-device",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn redirect_to_device_is_destructive() {
        assert_flags(
            "cat img.bin > /dev/sda",
            "redirect-to-block-device",
            RiskLevel::Destructive,
        );
        assert_safe("echo done > /dev/null");
    }

    // ── system ─────────────────────────────────────────────────────────

    #[test]
    fn auth_file_overwrite_is_destructive() {
        assert_flags(
            "echo 'evil ALL=(ALL) NOPASSWD:ALL' >> /etc/sudoers",
            "overwrite-auth-files",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn fork_bomb_is_destructive() {
        assert_flags(":(){ :|:& };:", "fork-bomb", RiskLevel::Destructive);
        assert_flags(":() { : | : & } ; :", "fork-bomb", RiskLevel::Destructive);
    }

    #[test]
    fn crontab_r_is_destructive() {
        assert_flags("crontab -r", "crontab-remove-all", RiskLevel::Destructive);
        assert_safe("crontab -l");
        assert_safe("crontab -e");
    }

    #[test]
    fn mv_to_dev_null_is_destructive() {
        assert_flags(
            "mv report.pdf /dev/null",
            "mv-to-dev-null",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn kill_pid_1_is_destructive() {
        assert_flags("kill -9 1", "kill-init", RiskLevel::Destructive);
        assert_safe("kill -9 12345");
    }

    #[test]
    fn chmod_777_broad_is_destructive() {
        assert_flags(
            "chmod -R 777 /",
            "chmod-world-writable-broad",
            RiskLevel::Destructive,
        );
        let scoped = "chmod -R 777 ./uploads";
        assert_flags(scoped, "chmod-world-writable", RiskLevel::Caution);
        assert_not_rule(scoped, "chmod-world-writable-broad");
        assert_safe("chmod 644 config.toml");
        assert_safe("chmod +x script.sh");
    }

    // ── caution class ──────────────────────────────────────────────────

    #[test]
    fn curl_pipe_shell_is_caution() {
        for cmd in [
            "curl -fsSL https://example.com/install.sh | sh",
            "curl https://example.com/x.sh | sudo bash",
            "wget -qO- https://example.com/setup | zsh",
        ] {
            assert_flags(cmd, "pipe-download-to-shell", RiskLevel::Caution);
        }
        // Downloading without executing is fine.
        assert_safe("curl -fsSL https://example.com/install.sh -o install.sh");
        // Piping into non-shell tools is fine.
        assert_safe("curl -s https://api.example.com/v1 | jq .name");
    }

    #[test]
    fn git_force_push_is_caution() {
        assert_flags(
            "git push --force origin main",
            "git-push-force",
            RiskLevel::Caution,
        );
        assert_flags("git push -f", "git-push-force", RiskLevel::Caution);
        // --force-with-lease is the safe variant and stays quiet.
        assert_safe("git push --force-with-lease origin main");
        assert_safe("git push origin main");
    }

    #[test]
    fn git_destructive_cleanup_is_caution() {
        assert_flags(
            "git reset --hard HEAD~3",
            "git-reset-hard",
            RiskLevel::Caution,
        );
        assert_flags("git clean -fdx", "git-clean-force", RiskLevel::Caution);
        assert_safe("git clean -n");
        assert_safe("git reset --soft HEAD~1");
    }

    #[test]
    fn sql_drop_is_caution() {
        assert_flags(
            "mysql -e 'DROP TABLE users;'",
            "sql-drop",
            RiskLevel::Caution,
        );
        assert_flags(
            "psql -c \"truncate table events\"",
            "sql-drop",
            RiskLevel::Caution,
        );
        assert_safe("mysql -e 'SELECT * FROM users LIMIT 10;'");
    }

    #[test]
    fn find_delete_is_caution() {
        assert_flags(
            "find . -name '*.tmp' -delete",
            "find-delete",
            RiskLevel::Caution,
        );
        assert_flags(
            "find /var/log -mtime +30 -exec rm {} \\;",
            "find-delete",
            RiskLevel::Caution,
        );
        assert_safe("find . -name '*.rs' -type f");
    }

    #[test]
    fn firewall_flush_is_caution() {
        assert_flags("iptables -F", "firewall-flush", RiskLevel::Caution);
        assert_flags("nft flush ruleset", "firewall-flush", RiskLevel::Caution);
        assert_safe("iptables -L -n -v");
    }

    #[test]
    fn power_commands_are_caution() {
        assert_flags("sudo reboot", "system-power", RiskLevel::Caution);
        assert_flags("shutdown -h now", "system-power", RiskLevel::Caution);
        // Words containing these strings must not match.
        assert_safe("grep -r reboot-notes ./docs");
    }

    #[test]
    fn shred_is_caution() {
        assert_flags(
            "shred -u secrets.txt",
            "shred-or-truncate",
            RiskLevel::Caution,
        );
        assert_flags(
            "truncate -s 0 app.log",
            "shred-or-truncate",
            RiskLevel::Caution,
        );
    }

    // ── everyday commands stay quiet ───────────────────────────────────

    #[test]
    fn common_commands_are_safe() {
        for cmd in [
            "ls -la",
            "rg -n TODO src/",
            "fd -e rs --changed-within 1d",
            "docker ps -a --filter status=exited",
            "tar --exclude=node_modules -czf archive.tar.gz .",
            "git status",
            "git commit -m 'feat: add safety analysis'",
            "cargo build --release",
            "du -sh * | sort -h",
            "ps aux | grep nginx",
            "kubectl get pods -n production",
            "ffmpeg -i in.mov -c:v libx264 out.mp4",
        ] {
            assert_safe(cmd);
        }
    }

    // ── assessment mechanics ───────────────────────────────────────────

    #[test]
    fn overall_level_is_most_severe_finding() {
        // Triggers both rm-recursive-force-broad (destructive) via `rm -rf /`
        // and git-reset-hard (caution).
        let a = assess("git reset --hard && rm -rf /");
        assert_eq!(a.level, RiskLevel::Destructive);
        assert!(a.findings.len() >= 2);
        // Most severe first.
        assert_eq!(a.findings[0].level, RiskLevel::Destructive);
    }

    #[test]
    fn safe_assessment_has_no_findings() {
        let a = assess("echo hello");
        assert!(a.is_safe());
        assert!(a.findings.is_empty());
        assert_eq!(a.level, RiskLevel::Safe);
    }

    #[test]
    fn risk_level_ordering() {
        assert!(RiskLevel::Safe < RiskLevel::Caution);
        assert!(RiskLevel::Caution < RiskLevel::Destructive);
    }

    #[test]
    fn assessment_serializes_for_ipc() {
        let a = assess("rm -rf /");
        let json = serde_json::to_string(&a).unwrap();
        let back: Assessment = serde_json::from_str(&json).unwrap();
        assert_eq!(back.level, RiskLevel::Destructive);
        assert_eq!(back.findings.len(), a.findings.len());
    }

    #[test]
    fn powershell_destructive_rules_are_table_driven() {
        let cases = [
            (
                "(RI 'HKLM:\\Software\\Incant' -Recurse)",
                "powershell-registry-change",
            ),
            (
                r#"reg.exe DELETE "HKCU\Software\Incant" /f"#,
                "windows-registry-native-change",
            ),
            ("$(& 'sPsV' -Name Spooler)", "powershell-service-change"),
            (
                "sc.exe config Spooler start= disabled",
                "windows-service-native-change",
            ),
            (
                "DISABLE-PNPDEVICE -InstanceId $device.InstanceId",
                "powershell-device-driver-change",
            ),
            (
                "pnputil.exe /delete-driver oem42.inf /uninstall",
                "windows-pnputil-change",
            ),
            (
                "Get-Disk -Number 2 | Clear-Disk -RemoveData",
                "powershell-storage-change",
            ),
            (
                "Remove-NetFirewallRule -DisplayName 'Incant'",
                "powershell-firewall-change",
            ),
            (
                "netsh.exe advfirewall reset",
                "windows-firewall-native-change",
            ),
            (
                "Set-DnsClientServerAddress -InterfaceIndex 4 -ServerAddresses 1.1.1.1",
                "powershell-network-change",
            ),
            (
                "netsh interface ipv4 set address name=Ethernet source=dhcp",
                "windows-netsh-network-change",
            ),
            (
                "& 'Clear-EventLog' -LogName System",
                "powershell-event-log-clear",
            ),
            (
                "wevtutil.EXE cl Microsoft-Windows-Diagnostics-Performance/Operational",
                "windows-event-log-native-clear",
            ),
            (
                "bcdedit.exe /deletevalue {current} safeboot",
                "windows-bcd-change",
            ),
            (
                "Set-MpPreference -DisableRealtimeMonitoring $true",
                "powershell-defender-change",
            ),
            (
                "Set-ExecutionPolicy -Scope LocalMachine -ExecutionPolicy RemoteSigned",
                "powershell-execution-policy-change",
            ),
            (
                "pwsh.exe -ExecutionPolicy BYPASS -File repair.ps1",
                "powershell-execution-policy-bypass",
            ),
            (
                "Register-ScheduledTask -TaskName Incant -Action $action",
                "powershell-scheduled-task-change",
            ),
            (
                "schtasks.exe /Delete /TN Incant /F",
                "windows-schtasks-change",
            ),
        ];

        for (command, rule) in cases {
            assert_flags(command, rule, RiskLevel::Destructive);
            for prefix in [
                "# Requires Administrator\n",
                "Get-Date\r\n",
                "Get-Date && ",
                "Get-Date || ",
                "Get-Date;",
                "Get-Date | ",
            ] {
                assert_flags(&format!("{prefix}{command}"), rule, RiskLevel::Destructive);
            }
        }

        for rule in RULES
            .iter()
            .filter(|rule| rule.segment_scoped && rule.level == RiskLevel::Destructive)
        {
            assert!(
                cases.iter().any(|(_, case_rule)| *case_rule == rule.id),
                "missing boundary coverage for {}",
                rule.id
            );
        }
    }

    #[test]
    fn powershell_whatif_suppresses_supported_destructive_cmdlets() {
        let cases = [
            (
                "Remove-Item -Path 'HKLM:\\Software\\Incant' -WhatIf",
                "powershell-registry-change",
            ),
            (
                "Stop-Service -Name Spooler -WhatIf",
                "powershell-service-change",
            ),
            (
                "Disable-PnpDevice -InstanceId abc -WhatIf:$true",
                "powershell-device-driver-change",
            ),
            (
                "Format-Volume -DriveLetter X -WhatIf",
                "powershell-storage-change",
            ),
            (
                "Remove-NetFirewallRule -Name Incant -WhatIf",
                "powershell-firewall-change",
            ),
            (
                "Remove-NetIPAddress -IPAddress 192.0.2.1 -WhatIf",
                "powershell-network-change",
            ),
            (
                "Clear-EventLog -LogName System -WhatIf",
                "powershell-event-log-clear",
            ),
            (
                "Set-MpPreference -DisableRealtimeMonitoring $true -WhatIf",
                "powershell-defender-change",
            ),
            (
                "Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -WhatIf",
                "powershell-execution-policy-change",
            ),
        ];

        for (command, rule) in cases {
            assert_not_rule(command, rule);
            assert_ne!(
                assess(command).level,
                RiskLevel::Destructive,
                "{command:?} must not be destructive with -WhatIf"
            );
        }
        assert_flags(
            "Stop-Service -Name Spooler -WhatIf:$false",
            "powershell-service-change",
            RiskLevel::Destructive,
        );
        assert_flags(
            "pnputil.exe /delete-driver oem42.inf -WhatIf",
            "windows-pnputil-change",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn powershell_escalation_signals_are_table_driven() {
        let cases = [
            ("Remove-Item file.txt -FoRcE", "powershell-force"),
            (
                "Stop-Service Spooler -Confirm : $FALSE",
                "powershell-confirm-false",
            ),
            (
                "$result | InVoKe-ExPrEsSiOn",
                "powershell-invoke-expression",
            ),
            ("$(iex $payload)", "powershell-invoke-expression"),
            (
                "powershell.exe -EncodedCommand SQBFAFgA",
                "powershell-encoded-command",
            ),
            ("pwsh -enc SQBFAFgA", "powershell-encoded-command"),
            (
                "$(& 'Start-Process' pwsh -Verb 'RunAs')",
                "powershell-runas",
            ),
        ];

        for (command, rule) in cases {
            assert_flags(command, rule, RiskLevel::Caution);
            for prefix in [
                "# preflight\n",
                "Get-Date\r\n",
                "Get-Date && ",
                "Get-Date || ",
                "Get-Date;",
                "Get-Date | ",
            ] {
                assert_flags(&format!("{prefix}{command}"), rule, RiskLevel::Caution);
            }
        }
    }

    #[test]
    fn powershell_whatif_is_scoped_to_the_matched_invocation() {
        for command in [
            "Stop-Service Spooler; Get-Service -WhatIf",
            "Stop-Service Spooler | Get-Service -WhatIf",
            "Stop-Service Spooler && Get-Service -WhatIf",
            "Stop-Service Spooler || Get-Service -WhatIf",
            "Stop-Service Spooler\nGet-Service -WhatIf",
            "Stop-Service Spooler\r\nGet-Service -WhatIf",
            "Stop-Service -Name '-WhatIf'",
            "Stop-Service Spooler # -WhatIf",
            "Write-Output -WhatIf $(Stop-Service Spooler)",
            "Write-Output $(Stop-Service Spooler) -WhatIf",
            "Write-Output $(Stop-Service Spooler -WhatIf) + $(Stop-Service Spooler)",
            "Remove-Item -Path HKLM:\\Software\\Incant; Get-Item -WhatIf",
        ] {
            assert_flags(
                command,
                if command.contains("Remove-Item") {
                    "powershell-registry-change"
                } else {
                    "powershell-service-change"
                },
                RiskLevel::Destructive,
            );
        }

        assert_not_rule(
            "Write-Output $(Stop-Service Spooler -WhatIf)",
            "powershell-service-change",
        );
        assert_not_rule(
            "Write-Output $(Stop-Service Spooler -WhatIf) + $(Stop-Service Spooler -WhatIf)",
            "powershell-service-change",
        );
        assert_flags(
            "pnputil.exe /delete-driver oem42.inf /uninstall; Get-PnpDevice -WhatIf",
            "windows-pnputil-change",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn powershell_recursive_remove_item_is_caution_with_adversarial_syntax() {
        for command in [
            "Remove-Item -LiteralPath '.\\cache' -Recurse",
            "Get-ChildItem .\\cache | DEL -r",
            "$(& 'rI' -Path '.\\cache' -RECURSE)",
            "(erase -r '.\\cache')",
            "rmdir '.\\cache' -r",
            "rm '.\\cache' -Recurse",
            "rd '.\\cache' -r",
            "# inventory only\r\nrm '.\\cache' -Recurse",
            "Get-ChildItem && rd '.\\cache' -r",
            "Get-ChildItem || rm '.\\cache' -r",
            "Get-ChildItem; Remove-Item '.\\cache' -Recurse",
        ] {
            assert_flags(
                command,
                "powershell-remove-item-recursive",
                RiskLevel::Caution,
            );
        }

        assert_not_rule(
            "Remove-Item -LiteralPath '.\\cache' -Recurse -WhatIf",
            "powershell-remove-item-recursive",
        );
        assert_flags(
            "Remove-Item -LiteralPath '.\\cache' -Recurse -WhatIf:$false",
            "powershell-remove-item-recursive",
            RiskLevel::Caution,
        );
        assert_safe("Remove-Item -LiteralPath '.\\single-file'");
        assert_safe("Write-Output 'Remove-Item C:\\data -Recurse'");

        for alias in ["ri", "rm", "rd"] {
            let registry = assess(&format!("{alias} 'HKLM:\\Software\\Incant' -r"));
            assert_eq!(registry.level, RiskLevel::Destructive);
            assert!(registry
                .findings
                .iter()
                .any(|finding| finding.rule == "powershell-remove-item-recursive"));
            assert!(registry
                .findings
                .iter()
                .any(|finding| finding.rule == "powershell-registry-change"));
        }
    }

    #[test]
    fn scheduled_task_mutations_are_destructive_and_case_insensitive() {
        let cases = [
            "Register-ScheduledTask -TaskName Incant -Action $action",
            "$(& 'UNREGISTER-SCHEDULEDTASK' -TaskName Incant -Confirm:$false)",
            "Set-ScheduledTask -TaskName Incant -Trigger $trigger",
            "Enable-ScheduledTask -TaskName Incant",
            "Disable-ScheduledTask -TaskName Incant",
        ];
        for command in cases {
            assert_flags(
                command,
                "powershell-scheduled-task-change",
                RiskLevel::Destructive,
            );
        }

        for command in [
            "schtasks.exe /Create /TN Incant /TR diagnostic.exe /SC ONCE /ST 23:59",
            "SCHTASKS /DELETE /TN Incant /F",
            "(schtasks.exe /Change /TN Incant /Disable)",
        ] {
            assert_flags(command, "windows-schtasks-change", RiskLevel::Destructive);
        }
    }

    #[test]
    fn scheduled_task_whatif_and_read_only_near_misses_are_handled() {
        for command in [
            "Register-ScheduledTask -TaskName Incant -Action $action -WhatIf",
            "Unregister-ScheduledTask -TaskName Incant -WhatIf:$true",
            "Set-ScheduledTask -TaskName Incant -Trigger $trigger -WhatIf",
            "Enable-ScheduledTask -TaskName Incant -WhatIf",
            "Disable-ScheduledTask -TaskName Incant -WhatIf",
        ] {
            assert_not_rule(command, "powershell-scheduled-task-change");
            assert_ne!(assess(command).level, RiskLevel::Destructive);
        }

        assert_flags(
            "Disable-ScheduledTask -TaskName Incant -WhatIf:$false",
            "powershell-scheduled-task-change",
            RiskLevel::Destructive,
        );
        assert_flags(
            "schtasks.exe /Delete /TN Incant /F -WhatIf",
            "windows-schtasks-change",
            RiskLevel::Destructive,
        );
        for command in [
            "Get-ScheduledTask -TaskName Incant",
            "schtasks.exe /Query /TN Incant /FO LIST",
            "Write-Output 'Unregister-ScheduledTask -TaskName Incant'",
            "Write-Output 'schtasks.exe /Delete /TN Incant'",
        ] {
            assert_safe(command);
        }
    }

    #[test]
    fn powershell_read_only_and_textual_near_misses_are_not_destructive() {
        let cases = [
            "Get-WinEvent -FilterHashtable @{ LogName = 'System'; Id = 41 }",
            "Get-CimInstance -ClassName Win32_OperatingSystem",
            "Get-PnpDevice -PresentOnly",
            "pnputil.exe /enum-drivers",
            "Get-Service -Name Spooler",
            "Get-Process -Name explorer",
            "Get-NetAdapter",
            "Resolve-DnsName example.com",
            "Test-NetConnection example.com -Port 443",
            "Get-ItemProperty -Path 'HKLM:\\Software\\Microsoft'",
            "reg.exe query HKLM\\Software\\Microsoft",
            "sc.exe query Spooler",
            "wevtutil.exe qe System /count:5",
            "bcdedit.exe /enum",
            "bcdedit.exe /export C:\\backup\\bcd",
            "Get-MpPreference",
            "Get-ExecutionPolicy -List",
            "Write-Output 'Stop-Service is destructive'",
            "Write-Output 'Set-MpPreference -DisableRealtimeMonitoring'",
            "Get-ChildItem -Force",
        ];

        for command in cases {
            assert_ne!(
                assess(command).level,
                RiskLevel::Destructive,
                "{command:?} must remain non-destructive"
            );
        }
    }
}
