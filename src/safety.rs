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
}

impl Rule {
    fn new(
        id: &'static str,
        level: RiskLevel,
        reason: &'static str,
        all: &[&str],
        unless: Option<&str>,
    ) -> Self {
        Self {
            id,
            level,
            reason,
            all: all
                .iter()
                .map(|p| Regex::new(p).expect("static safety pattern must compile"))
                .collect(),
            unless: unless.map(|p| Regex::new(p).expect("static safety pattern must compile")),
        }
    }

    fn matches(&self, command: &str) -> bool {
        self.all.iter().all(|re| re.is_match(command))
            && self.unless.as_ref().is_none_or(|re| !re.is_match(command))
    }
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
}
