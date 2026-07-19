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
    fn specialized_match(
        &self,
        source: &PowerShellSource<'_>,
        operation_start: usize,
    ) -> Option<bool> {
        let launcher_start = || {
            powershell_launcher_start(source, operation_start)
                .map(|(launcher_start, _)| launcher_start)
        };
        match self.id {
            "powershell-encoded-command" => Some(
                launcher_start()
                    .is_some_and(|start| matches_encoded_powershell_command(source, start)),
            ),
            "powershell-execution-policy-bypass" => Some(
                launcher_start()
                    .is_some_and(|start| matches_execution_policy_bypass(source, start)),
            ),
            _ => None,
        }
    }

    fn matches(&self, command: &str, sources: &[PowerShellSource<'_>]) -> bool {
        if !self.segment_scoped {
            return self.all.iter().all(|re| re.is_match(command))
                && self.unless.as_ref().is_none_or(|re| !re.is_match(command));
        }

        sources.iter().any(|source| {
            source.lex.command_starts.iter().any(|&original_start| {
                if let Some(specialized) = self.specialized_match(source, original_start) {
                    return specialized;
                }

                let start = source.normalized_boundary(original_start);
                let untrimmed = &source.normalized[start..];
                let candidate = untrimmed.trim_start_matches(char::is_whitespace);
                let candidate_start = start + (untrimmed.len() - candidate.len());
                let Some(primary) = self.all.first() else {
                    return false;
                };
                if !self.all[1..].iter().all(|re| re.is_match(candidate)) {
                    return false;
                }

                primary.find_iter(candidate).any(|matched| {
                    let normalized_start = candidate_start + matched.start();
                    let normalized_end = candidate_start + matched.end();
                    let Some((match_start, match_end)) =
                        source.original_range(normalized_start, normalized_end)
                    else {
                        return false;
                    };
                    let Some(operation_start) =
                        source
                            .lex
                            .operation_start(source.text, match_start, match_end)
                    else {
                        return false;
                    };
                    if powershell_operation_is_inert_assignment(
                        source.text,
                        &source.lex,
                        operation_start,
                    ) {
                        return false;
                    }
                    let invocation_end = source.lex.invocation_end(source.text, operation_start);
                    let normalized_operation = source.normalized_boundary(operation_start);
                    let normalized_invocation_end = source.normalized_boundary(invocation_end);
                    self.unless.as_ref().is_none_or(|unless| {
                        !unless
                            .find_iter(
                                &source.normalized[normalized_operation..normalized_invocation_end],
                            )
                            .any(|exception| {
                                let start = normalized_operation + exception.start();
                                let end = normalized_operation + exception.end();
                                source.original_range(start, end).is_some_and(
                                    |(exception_start, exception_end)| {
                                        source
                                            .lex
                                            .operation_start(
                                                source.text,
                                                exception_start,
                                                exception_end,
                                            )
                                            .is_some_and(|exception_operation| {
                                                source.lex.bytes[exception_operation].nesting
                                                    == source.lex.bytes[operation_start].nesting
                                            })
                                    },
                                )
                            })
                    })
                })
            })
        })
    }
}

fn powershell_operation_is_inert_assignment(
    command: &str,
    lex: &PowerShellLex,
    operation_start: usize,
) -> bool {
    let raw = command.as_bytes();
    let mut closed_braces = 0_u16;
    for index in (0..operation_start).rev() {
        if !lex.bytes[index].executable {
            continue;
        }
        match raw[index] {
            b'}' => closed_braces += 1,
            b'{' if closed_braces > 0 => closed_braces -= 1,
            b'{' => {
                return previous_executable_non_whitespace(command, &lex.bytes, index)
                    == Some(b'=');
            }
            _ => {}
        }
    }
    false
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PowerShellNesting {
    paren: u16,
    brace: u16,
    bracket: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct PowerShellByte {
    executable: bool,
    invocation_quoted: bool,
    escaped: bool,
    separator: bool,
    nesting: PowerShellNesting,
}

struct PowerShellLex {
    bytes: Vec<PowerShellByte>,
    command_starts: Vec<usize>,
}

struct PowerShellSource<'a> {
    text: &'a str,
    normalized: String,
    normalized_offsets: Vec<usize>,
    lex: PowerShellLex,
}

impl<'a> PowerShellSource<'a> {
    fn new(text: &'a str) -> Self {
        let lex = PowerShellLex::new(text);
        let (normalized, normalized_offsets) = normalize_powershell_executable(text, &lex);
        Self {
            text,
            normalized,
            normalized_offsets,
            lex,
        }
    }

    fn normalized_boundary(&self, original: usize) -> usize {
        self.normalized_offsets
            .partition_point(|&offset| offset < original)
    }

    fn original_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end {
            return None;
        }
        let original_start = *self.normalized_offsets.get(start)?;
        let original_end = self
            .normalized_offsets
            .get(end - 1)
            .copied()
            .map(|offset| offset + 1)?;
        Some((original_start, original_end))
    }
}

impl PowerShellLex {
    /// Record which bytes are executable PowerShell and their lexical nesting.
    /// This intentionally handles only syntax needed by the advisory rules:
    /// strings, comments, backtick escapes, grouping, and command separators.
    fn new(command: &str) -> Self {
        let raw = command.as_bytes();
        let mut bytes = vec![PowerShellByte::default(); raw.len()];
        let mut command_starts = vec![0];
        let mut nesting = PowerShellNesting::default();
        let mut quote: Option<(u8, bool)> = None;
        let mut line_comment = false;
        let mut block_comment = false;
        let mut index = 0;

        while index < raw.len() {
            bytes[index].nesting = nesting;

            if line_comment {
                if matches!(raw[index], b'\r' | b'\n') {
                    line_comment = false;
                } else {
                    index += 1;
                    continue;
                }
            }
            if block_comment {
                if raw[index] == b'#' && raw.get(index + 1) == Some(&b'>') {
                    if index + 1 < bytes.len() {
                        bytes[index + 1].nesting = nesting;
                    }
                    block_comment = false;
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }

            if let Some((delimiter, invocation_quoted)) = quote {
                bytes[index].invocation_quoted = invocation_quoted;
                if delimiter == b'\'' && raw[index] == b'\'' {
                    if raw.get(index + 1) == Some(&b'\'') {
                        bytes[index + 1].nesting = nesting;
                        bytes[index + 1].invocation_quoted = invocation_quoted;
                        index += 2;
                        continue;
                    }
                    quote = None;
                } else if delimiter == b'"' {
                    if raw[index] == b'`' && index + 1 < raw.len() {
                        bytes[index + 1].nesting = nesting;
                        bytes[index + 1].invocation_quoted = invocation_quoted;
                        bytes[index + 1].escaped = true;
                        index += 2;
                        continue;
                    }
                    if raw[index] == b'"' {
                        quote = None;
                    }
                }
                index += 1;
                continue;
            }

            if raw[index] == b'<' && raw.get(index + 1) == Some(&b'#') {
                if index + 1 < bytes.len() {
                    bytes[index + 1].nesting = nesting;
                }
                block_comment = true;
                index += 2;
                continue;
            }
            if raw[index] == b'#' {
                line_comment = true;
                index += 1;
                continue;
            }
            if let Some(delimiter) = powershell_here_string_opener(raw, index) {
                let end = powershell_here_string_end(raw, index + 2, delimiter)
                    .map_or(raw.len(), |terminator| terminator + 2);
                for byte in &mut bytes[index..end] {
                    byte.nesting = nesting;
                }
                index = end;
                continue;
            }
            if raw[index] == b'`' && index + 1 < raw.len() {
                let escaped_len = if raw[index + 1] == b'\r' && raw.get(index + 2) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                for offset in 1..=escaped_len {
                    bytes[index + offset].nesting = nesting;
                    bytes[index + offset].escaped = true;
                    if !matches!(raw[index + offset], b'\r' | b'\n') {
                        bytes[index + offset].executable = true;
                    }
                }
                index += escaped_len + 1;
                continue;
            }
            if matches!(raw[index], b'\'' | b'"') {
                let invocation_quoted =
                    previous_executable_non_whitespace(command, &bytes, index) == Some(b'&');
                bytes[index].invocation_quoted = invocation_quoted;
                quote = Some((raw[index], invocation_quoted));
                index += 1;
                continue;
            }

            bytes[index].executable = true;
            let mut separator_len = powershell_separator_len(raw, index);
            if powershell_is_background_operator(command, &bytes, &command_starts, index) {
                separator_len = 1;
            }
            if separator_len != 0 {
                for offset in 0..separator_len {
                    bytes[index + offset].executable = true;
                    bytes[index + offset].separator = true;
                    bytes[index + offset].nesting = nesting;
                }
                command_starts.push(index + separator_len);
                index += separator_len;
                continue;
            }

            match raw[index] {
                b'(' => {
                    nesting.paren = nesting.paren.saturating_add(1);
                    command_starts.push(index + 1);
                }
                b')' => nesting.paren = nesting.paren.saturating_sub(1),
                b'{' => {
                    nesting.brace = nesting.brace.saturating_add(1);
                    command_starts.push(index + 1);
                }
                b'}' => nesting.brace = nesting.brace.saturating_sub(1),
                b'[' => nesting.bracket = nesting.bracket.saturating_add(1),
                b']' => nesting.bracket = nesting.bracket.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }

        command_starts.sort_unstable();
        command_starts.dedup();
        Self {
            bytes,
            command_starts,
        }
    }

    /// Locate the operation token in a regex match. Tokens in ordinary strings
    /// or comments are inert; a quoted executable immediately invoked by `&`
    /// is executable code.
    fn operation_start(&self, command: &str, start: usize, end: usize) -> Option<usize> {
        let raw = command.as_bytes();
        (start..end.min(self.bytes.len())).find(|&index| {
            let byte = self.bytes[index];
            let token_byte = raw[index];
            (byte.executable || byte.invocation_quoted)
                && (token_byte.is_ascii_alphanumeric() || matches!(token_byte, b'_' | b'-'))
        })
    }

    /// End the matched invocation at a separator at the operation's nesting
    /// depth, or at the delimiter that exits its containing scope.
    fn invocation_end(&self, command: &str, operation_start: usize) -> usize {
        let nesting = self.bytes[operation_start].nesting;
        let raw = command.as_bytes();
        let mut index = operation_start;
        while index < self.bytes.len() {
            let byte = self.bytes[index];
            if !byte.executable {
                index += 1;
                continue;
            }
            if byte.nesting == nesting && byte.separator {
                return index;
            }
            if (raw[index] == b')' && nesting.paren > 0 && byte.nesting.paren == nesting.paren)
                || (raw[index] == b'}' && nesting.brace > 0 && byte.nesting.brace == nesting.brace)
                || (raw[index] == b']'
                    && nesting.bracket > 0
                    && byte.nesting.bracket == nesting.bracket)
            {
                return index;
            }
            index += 1;
        }
        self.bytes.len()
    }
}
fn normalize_powershell_executable(command: &str, lex: &PowerShellLex) -> (String, Vec<usize>) {
    let raw = command.as_bytes();
    let mut normalized = Vec::with_capacity(raw.len());
    let mut offsets = Vec::with_capacity(raw.len());
    for (index, &value) in raw.iter().enumerate() {
        let byte = lex.bytes[index];
        if value == b'`' && lex.bytes.get(index + 1).is_some_and(|next| next.escaped) {
            continue;
        }
        if byte.escaped {
            if matches!(value, b'\r' | b'\n') {
                continue;
            }
            if value.is_ascii_whitespace()
                || matches!(value, b';' | b'&' | b'|' | b'{' | b'}' | b'(' | b')')
            {
                normalized.push(b'_');
                offsets.push(index);
                continue;
            }
        }
        if byte.invocation_quoted && matches!(value, b'\'' | b'"') {
            continue;
        }
        normalized.push(value);
        offsets.push(index);
    }
    (
        String::from_utf8(normalized).expect("removing ASCII escape bytes preserves UTF-8"),
        offsets,
    )
}
fn powershell_at_command_start(
    command: &str,
    bytes: &[PowerShellByte],
    command_starts: &[usize],
    index: usize,
) -> bool {
    let start = command_starts
        .iter()
        .rev()
        .find(|&&start| start <= index)
        .copied()
        .unwrap_or(0);
    !(start..index)
        .any(|offset| bytes[offset].executable && !command.as_bytes()[offset].is_ascii_whitespace())
}
fn powershell_is_background_operator(
    command: &str,
    bytes: &[PowerShellByte],
    command_starts: &[usize],
    index: usize,
) -> bool {
    let raw = command.as_bytes();
    raw.get(index) == Some(&b'&')
        && raw.get(index + 1) != Some(&b'&')
        && !powershell_at_command_start(command, bytes, command_starts, index)
        && previous_executable_non_whitespace(command, bytes, index) != Some(b'>')
}

fn powershell_here_string_opener(raw: &[u8], index: usize) -> Option<u8> {
    if raw.get(index) != Some(&b'@') {
        return None;
    }
    let delimiter = *raw.get(index + 1)?;
    if !matches!(delimiter, b'\'' | b'"') {
        return None;
    }
    let mut cursor = index + 2;
    while matches!(raw.get(cursor), Some(b' ' | b'\t')) {
        cursor += 1;
    }
    matches!(raw.get(cursor), None | Some(b'\r' | b'\n')).then_some(delimiter)
}

fn powershell_here_string_end(raw: &[u8], body_start: usize, delimiter: u8) -> Option<usize> {
    let mut line_start = body_start;
    if raw.get(line_start) == Some(&b'\r') {
        line_start += 1;
    }
    if raw.get(line_start) == Some(&b'\n') {
        line_start += 1;
    }
    while line_start < raw.len() {
        if raw.get(line_start) == Some(&delimiter)
            && raw.get(line_start + 1) == Some(&b'@')
            && matches!(raw.get(line_start + 2), None | Some(b'\r' | b'\n'))
        {
            return Some(line_start);
        }
        let newline = raw[line_start..].iter().position(|&byte| byte == b'\n')?;
        line_start += newline + 1;
    }
    None
}

fn previous_executable_non_whitespace(
    command: &str,
    bytes: &[PowerShellByte],
    before: usize,
) -> Option<u8> {
    (0..before).rev().find_map(|index| {
        let raw = command.as_bytes()[index];
        (bytes[index].executable && !raw.is_ascii_whitespace()).then_some(raw)
    })
}

fn powershell_separator_len(raw: &[u8], index: usize) -> usize {
    match raw[index] {
        b'\r' => usize::from(raw.get(index + 1) == Some(&b'\n')) + 1,
        b'\n' | b';' => 1,
        b'&' if raw.get(index + 1) == Some(&b'&') => 2,
        b'|' => usize::from(raw.get(index + 1) == Some(&b'|')) + 1,
        _ => 0,
    }
}

fn powershell_sources(command: &str) -> Vec<PowerShellSource<'_>> {
    let mut pending = vec![command];
    let mut sources = Vec::new();
    let mut index = 0;

    while index < pending.len() {
        let text = pending[index];
        let source = PowerShellSource::new(text);
        if pending.len() < 16 {
            let mut discovered = nested_powershell_payloads(text, &source);
            discovered.extend(expandable_string_subexpressions(text));
            for payload in discovered {
                let duplicate = pending.iter().any(|known| {
                    known.as_ptr() == payload.as_ptr() && known.len() == payload.len()
                });
                if !duplicate {
                    pending.push(payload);
                }
            }
        }
        sources.push(source);
        index += 1;
    }
    sources
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerShellLauncherParameter {
    Command,
    CommandWithArgs,
    EncodedCommand,
    ExecutionPolicy,
}

#[derive(Clone, Copy, Debug)]
struct PowerShellLauncherOption {
    parameter: PowerShellLauncherParameter,
    operation_start: usize,
    argument_start: usize,
    invocation_end: usize,
}

fn nested_powershell_payloads<'a>(command: &'a str, source: &PowerShellSource<'_>) -> Vec<&'a str> {
    powershell_launcher_options(source)
        .into_iter()
        .filter_map(|option| {
            matches!(
                option.parameter,
                PowerShellLauncherParameter::Command | PowerShellLauncherParameter::CommandWithArgs
            )
            .then(|| {
                powershell_launcher_payload(command, option.argument_start, option.invocation_end)
            })
            .flatten()
        })
        .collect()
}

fn powershell_launcher_payload(
    command: &str,
    argument_start: usize,
    invocation_end: usize,
) -> Option<&str> {
    if matches!(command.as_bytes()[argument_start], b'\'' | b'"') {
        let end = powershell_quote_end(command, argument_start)?;
        (end <= invocation_end).then_some(&command[argument_start + 1..end])
    } else {
        Some(command[argument_start..invocation_end].trim_end())
    }
}

fn powershell_launcher_options(source: &PowerShellSource<'_>) -> Vec<PowerShellLauncherOption> {
    let mut options = Vec::new();
    for &start in &source.lex.command_starts {
        let Some((operation_start, mut cursor)) = powershell_launcher_start(source, start) else {
            continue;
        };
        let invocation_end = source.lex.invocation_end(source.text, operation_start);
        while cursor < invocation_end {
            cursor = skip_powershell_whitespace(source.text, cursor, invocation_end);
            if cursor >= invocation_end {
                break;
            }
            let token_start = cursor;
            while cursor < invocation_end {
                let byte = source.lex.bytes[cursor];
                let raw = source.text.as_bytes()[cursor];
                if raw == b'`'
                    && source
                        .lex
                        .bytes
                        .get(cursor + 1)
                        .is_some_and(|next| next.escaped)
                {
                    cursor += 2;
                    continue;
                }
                if !byte.executable || raw.is_ascii_whitespace() {
                    break;
                }
                cursor += 1;
            }
            if cursor == token_start {
                cursor += 1;
                continue;
            }
            let normalized_start = source.normalized_boundary(token_start);
            let normalized_end = source.normalized_boundary(cursor);
            let token = &source.normalized[normalized_start..normalized_end];
            let Some(parameter) = token
                .strip_prefix('-')
                .and_then(resolve_powershell_launcher_parameter)
            else {
                continue;
            };
            let argument_start = skip_powershell_whitespace(source.text, cursor, invocation_end);
            if argument_start < invocation_end {
                options.push(PowerShellLauncherOption {
                    parameter,
                    operation_start,
                    argument_start,
                    invocation_end,
                });
            }
            break;
        }
    }
    options
}

fn powershell_launcher_start(
    source: &PowerShellSource<'_>,
    start: usize,
) -> Option<(usize, usize)> {
    let limit = source.text.len();
    let mut cursor = skip_powershell_whitespace(source.text, start, limit);
    if source.text.as_bytes().get(cursor) == Some(&b'&') && !source.lex.bytes[cursor].separator {
        cursor = skip_powershell_whitespace(source.text, cursor + 1, limit);
    }
    let operation_start = cursor;
    let (token_start, token_end) =
        if matches!(source.text.as_bytes().get(cursor), Some(b'\'' | b'"')) {
            if !source.lex.bytes[cursor].invocation_quoted {
                return None;
            }
            let end = powershell_quote_end(source.text, cursor)?;
            (cursor + 1, end)
        } else {
            let start = cursor;
            while cursor < limit {
                let byte = source.lex.bytes[cursor];
                let raw = source.text.as_bytes()[cursor];
                if raw == b'`'
                    && source
                        .lex
                        .bytes
                        .get(cursor + 1)
                        .is_some_and(|next| next.escaped)
                {
                    cursor += 2;
                    continue;
                }
                if !byte.executable || raw.is_ascii_whitespace() || byte.separator {
                    break;
                }
                cursor += 1;
            }
            (start, cursor)
        };
    let normalized_start = source.normalized_boundary(token_start);
    let normalized_end = source.normalized_boundary(token_end);
    let executable = &source.normalized[normalized_start..normalized_end];
    is_powershell_executable(executable).then_some((
        operation_start,
        token_end
            + usize::from(
                source
                    .text
                    .as_bytes()
                    .get(token_end)
                    .is_some_and(|byte| matches!(byte, b'\'' | b'"')),
            ),
    ))
}

fn is_powershell_executable(token: &str) -> bool {
    let basename = token
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(token)
        .trim_end_matches(['\'', '"']);
    basename.eq_ignore_ascii_case("pwsh")
        || basename.eq_ignore_ascii_case("pwsh.exe")
        || basename.eq_ignore_ascii_case("powershell")
        || basename.eq_ignore_ascii_case("powershell.exe")
}

fn skip_powershell_whitespace(command: &str, mut cursor: usize, limit: usize) -> usize {
    while cursor < limit && command.as_bytes()[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

fn resolve_powershell_launcher_parameter(value: &str) -> Option<PowerShellLauncherParameter> {
    let value = value.to_ascii_lowercase();
    if value == "c" || (value.len() >= 2 && "command".starts_with(&value)) {
        return Some(PowerShellLauncherParameter::Command);
    }
    if value == "cwa" || value == "commandwithargs" {
        return Some(PowerShellLauncherParameter::CommandWithArgs);
    }
    if value == "e"
        || value == "ec"
        || (value.len() >= "en".len() && "encodedcommand".starts_with(&value))
    {
        return Some(PowerShellLauncherParameter::EncodedCommand);
    }
    if value == "ep" || (value.len() >= "ex".len() && "executionpolicy".starts_with(&value)) {
        return Some(PowerShellLauncherParameter::ExecutionPolicy);
    }
    None
}
fn matches_encoded_powershell_command(
    source: &PowerShellSource<'_>,
    operation_start: usize,
) -> bool {
    powershell_launcher_options(source).iter().any(|option| {
        option.operation_start == operation_start
            && option.parameter == PowerShellLauncherParameter::EncodedCommand
    })
}

fn matches_execution_policy_bypass(source: &PowerShellSource<'_>, operation_start: usize) -> bool {
    powershell_launcher_options(source).iter().any(|option| {
        option.operation_start == operation_start
            && option.parameter == PowerShellLauncherParameter::ExecutionPolicy
            && powershell_launcher_argument(
                source.text,
                option.argument_start,
                option.invocation_end,
            )
            .is_some_and(|argument| {
                argument.eq_ignore_ascii_case("bypass")
                    || argument.eq_ignore_ascii_case("unrestricted")
            })
    })
}

fn powershell_launcher_argument(
    command: &str,
    argument_start: usize,
    invocation_end: usize,
) -> Option<&str> {
    let raw = command.as_bytes();
    if matches!(raw.get(argument_start), Some(b'\'' | b'"')) {
        let end = powershell_quote_end(command, argument_start)?;
        return (end <= invocation_end).then_some(&command[argument_start + 1..end]);
    }
    let end = command[argument_start..invocation_end]
        .find(char::is_whitespace)
        .map_or(invocation_end, |offset| argument_start + offset);
    Some(&command[argument_start..end])
}

fn powershell_quote_end(command: &str, quote_start: usize) -> Option<usize> {
    let raw = command.as_bytes();
    let quote = *raw.get(quote_start)?;
    let mut index = quote_start + 1;
    while index < raw.len() {
        if quote == b'\'' && raw[index] == b'\'' && raw.get(index + 1) == Some(&b'\'') {
            index += 2;
            continue;
        }
        if quote == b'"' && raw[index] == b'`' && index + 1 < raw.len() {
            index += 2;
            continue;
        }
        if raw[index] == quote {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn expandable_string_subexpressions(command: &str) -> Vec<&str> {
    let raw = command.as_bytes();
    let mut payloads = Vec::new();
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = 0;

    while index < raw.len() {
        if line_comment {
            if matches!(raw[index], b'\r' | b'\n') {
                line_comment = false;
            } else {
                index += 1;
                continue;
            }
        }
        if block_comment {
            if raw[index] == b'#' && raw.get(index + 1) == Some(&b'>') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

        match quote {
            Some(b'\'') => {
                if raw[index] == b'\'' {
                    if raw.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    quote = None;
                }
            }
            Some(b'"') => {
                if raw[index] == b'`' && index + 1 < raw.len() {
                    index += 2;
                    continue;
                }
                if raw[index] == b'"' {
                    quote = None;
                } else if raw[index] == b'$' && raw.get(index + 1) == Some(&b'(') {
                    if let Some(end) = powershell_subexpression_end(command, index + 1) {
                        payloads.push(&command[index + 2..end]);
                        index = end;
                    }
                }
            }
            Some(_) => unreachable!("only PowerShell quote bytes are stored"),
            None => {
                if let Some(delimiter) = powershell_here_string_opener(raw, index) {
                    let terminator = powershell_here_string_end(raw, index + 2, delimiter);
                    if delimiter == b'"' {
                        let limit = terminator.unwrap_or(raw.len());
                        let mut cursor = index + 2;
                        while cursor < limit {
                            if raw[cursor] == b'`' && cursor + 1 < limit {
                                cursor += 2;
                                continue;
                            }
                            if raw[cursor] == b'$' && raw.get(cursor + 1) == Some(&b'(') {
                                if let Some(end) = powershell_subexpression_end(command, cursor + 1)
                                {
                                    if end <= limit {
                                        payloads.push(&command[cursor + 2..end]);
                                        cursor = end + 1;
                                        continue;
                                    }
                                }
                            }
                            cursor += 1;
                        }
                    }
                    index = terminator.map_or(raw.len(), |end| end + 2);
                    continue;
                }
                if raw[index] == b'<' && raw.get(index + 1) == Some(&b'#') {
                    block_comment = true;
                    index += 2;
                    continue;
                }
                if raw[index] == b'#' {
                    line_comment = true;
                } else if raw[index] == b'`' && index + 1 < raw.len() {
                    index += 2;
                    continue;
                } else if matches!(raw[index], b'\'' | b'"') {
                    quote = Some(raw[index]);
                }
            }
        }
        index += 1;
    }
    payloads
}

fn powershell_subexpression_end(command: &str, open_paren: usize) -> Option<usize> {
    let raw = command.as_bytes();
    let mut depth = 1_u16;
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = open_paren + 1;

    while index < raw.len() {
        if line_comment {
            if matches!(raw[index], b'\r' | b'\n') {
                line_comment = false;
            } else {
                index += 1;
                continue;
            }
        }
        if block_comment {
            if raw[index] == b'#' && raw.get(index + 1) == Some(&b'>') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

        match quote {
            Some(b'\'') => {
                if raw[index] == b'\'' {
                    if raw.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    quote = None;
                }
            }
            Some(b'"') => {
                if raw[index] == b'`' && index + 1 < raw.len() {
                    index += 2;
                    continue;
                }
                if raw[index] == b'"' {
                    quote = None;
                } else if raw[index] == b'$' && raw.get(index + 1) == Some(&b'(') {
                    depth = depth.saturating_add(1);
                    index += 2;
                    continue;
                }
            }
            Some(_) => unreachable!("only PowerShell quote bytes are stored"),
            None => {
                if let Some(delimiter) = powershell_here_string_opener(raw, index) {
                    index = powershell_here_string_end(raw, index + 2, delimiter)
                        .map_or(raw.len(), |terminator| terminator + 2);
                    continue;
                }
                if raw[index] == b'<' && raw.get(index + 1) == Some(&b'#') {
                    block_comment = true;
                    index += 2;
                    continue;
                }
                if raw[index] == b'#' {
                    line_comment = true;
                } else if raw[index] == b'`' && index + 1 < raw.len() {
                    index += 2;
                    continue;
                } else if matches!(raw[index], b'\'' | b'"') {
                    quote = Some(raw[index]);
                } else if raw[index] == b'(' {
                    depth = depth.saturating_add(1);
                } else if raw[index] == b')' {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
            }
        }
        index += 1;
    }
    None
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
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:remove-item|ri|rm|del|erase|rd|rmdir|remove-itemproperty|rp|clear-item|cli|clear-itemproperty|clp|set-item|si|set-itemproperty|sp|new-item|ni|new-itemproperty|rename-item|rni|rename-itemproperty|rnp|move-item|mi|move-itemproperty|mp|copy-item|cpi)\b['"]?[^|;\r\n]*(?:registry::|hk(?:lm|cu|cr|u|cc):)"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "windows-registry-native-change",
            RiskLevel::Destructive,
            "changes or removes Windows registry keys or values",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*(?:[^|;\r\n]*[\\/])?)?reg(?:\.exe)?\s+(?:add|delete|import|restore|load|unload)\b"],
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
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*(?:[^|;\r\n]*[\\/])?)?schtasks(?:\.exe)?\b[^|;\r\n]*/(?:create|delete|change)\b"],
            None,
        ),
        Rule::new(
            "windows-service-native-change",
            RiskLevel::Destructive,
            "changes, stops, or removes a Windows service",
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*(?:[^|;\r\n]*[\\/])?)?sc\.exe\s+(?:stop|delete|config|failure|failureflag)\b"],
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
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:clear-disk|initialize-disk|format-volume|new-partition|remove-partition|resize-partition|set-partition|set-disk)\b['"]?"#],
            Some(POWERSHELL_WHAT_IF),
        ),
        Rule::new(
            "powershell-firewall-change",
            RiskLevel::Destructive,
            "changes or removes Windows Firewall policy",
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:(?:new|set|remove|disable|enable)-netfirewall(?:rule|profile))\b['"]?"#],
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
            &[r"(?i)(?:^|[|;{(]\s*)(?:&\s*(?:[^|;\r\n]*[\\/])?)?wevtutil(?:\.exe)?\s+(?:cl|clear-log)\b"],
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
            &[r#"(?i)(?:^|[|;{(]\s*)(?:&\s*['"]?)?(?:start-process|saps)\b['"]?[^|;\r\n]*-verb\s+['"]?runas\b"#],
            None,
        ),
    ]
});

/// Assess a generated command against the rule table.
///
/// Every matching rule becomes a [`Finding`]; the overall level is the most
/// severe finding (or [`RiskLevel::Safe`] when nothing matches).
pub fn assess(command: &str) -> Assessment {
    let sources = powershell_sources(command);
    let mut findings: Vec<Finding> = RULES
        .iter()
        .filter(|rule| rule.matches(command, &sources))
        .map(|rule| Finding {
            rule: rule.id.to_string(),
            level: rule.level,
            reason: rule.reason.to_string(),
        })
        .collect();

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
    #[test]
    fn powershell_lexer_rejects_inert_text_and_scopes_nested_whatif() {
        for command in [
            "Write-Output 'pnputil.exe /delete-driver oem42.inf /uninstall'",
            "Write-Output 'netsh winsock reset'",
            "Write-Output 'bcdedit.exe /deletevalue {current} safeboot'",
            "Write-Output 'pwsh.exe -EncodedCommand SQBFAFgA'",
            "'diagnostic; Stop-Service Spooler'",
            "# Stop-Service Spooler\r\nGet-Service Spooler",
            "Write-Output \"diagnostic `\"Stop-Service Spooler`\"\"",
            "<# Stop-Service Spooler #> Get-Service Spooler",
            "Write-Output \"pwsh -Command 'Stop-Service Spooler'\"",
            "pwsh -Command 'Stop-Service Spooler -WhatIf'",
        ] {
            assert_safe(command);
        }

        for command in [
            "& { param([switch]$WhatIf) Get-Date; Stop-Service Spooler } -WhatIf",
            "& { Get-Date | Stop-Service Spooler } -WhatIf",
            "Get-Date\r\nStop-Service Spooler",
            "Remove-Item -Path 'HKLM:\\Software\\Incant' -Recurse",
            "& 'pnputil.exe' /delete-driver oem42.inf /uninstall",
            "pwsh -NoProfile -Command 'Stop-Service Spooler'",
        ] {
            assert_eq!(
                assess(command).level,
                RiskLevel::Destructive,
                "{command:?} must remain destructive"
            );
        }

        assert_not_rule(
            "& { Get-Date; Stop-Service Spooler -WhatIf } -WhatIf:$false",
            "powershell-service-change",
        );
        assert_flags(
            "& { Get-Date; Stop-Service Spooler -WhatIf:$false } -WhatIf",
            "powershell-service-change",
            RiskLevel::Destructive,
        );
    }
    #[test]
    fn executable_subexpressions_and_unquoted_nested_commands_are_assessed() {
        for command in [
            "Write-Output \"$(Stop-Service Spooler)\"",
            "Write-Output \"$(& { Stop-Service Spooler })\"",
            "pwsh -Command Stop-Service Spooler",
            "powershell.exe -c Stop-Service Spooler",
            "pwsh -CommandWithArgs { Stop-Service Spooler } ignored",
            "pwsh -Command Stop-Service Spooler; Get-Service -WhatIf",
        ] {
            assert_flags(command, "powershell-service-change", RiskLevel::Destructive);
        }

        for command in [
            "Write-Output '$(Stop-Service Spooler)'",
            "Write-Output \"`$(Stop-Service Spooler)\"",
            "Write-Output \"$(Write-Output 'Stop-Service Spooler')\"",
            "Write-Output \"$([string]'Stop-Service Spooler')\"",
            "Write-Output 'pwsh -Command Stop-Service Spooler'",
            "pwsh -Command Write-Output 'Stop-Service Spooler'",
            "pwsh -Command Stop-Service Spooler -WhatIf; Get-Date",
            "pwsh -File diagnostics.ps1 -CommandText Stop-Service",
        ] {
            assert_safe(command);
        }
    }

    #[test]
    fn additional_registry_storage_and_firewall_mutations_are_covered() {
        let destructive = [
            (
                "Rename-ItemProperty -Path 'HKLM:\\Software\\Incant' -Name Old -NewName New",
                "powershell-registry-change",
            ),
            (
                "rnp 'HKCU:\\Software\\Incant' Old New",
                "powershell-registry-change",
            ),
            (
                "Move-ItemProperty -Path 'HKLM:\\Software\\Incant' -Name Value -Destination 'HKCU:\\Software\\Incant'",
                "powershell-registry-change",
            ),
            (
                "mp 'HKCU:\\Software\\Incant' Value 'HKLM:\\Software\\Incant'",
                "powershell-registry-change",
            ),
            (
                "New-Partition -DiskNumber 2 -UseMaximumSize",
                "powershell-storage-change",
            ),
            (
                "Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'",
                "powershell-firewall-change",
            ),
        ];
        for (command, rule) in destructive {
            assert_flags(command, rule, RiskLevel::Destructive);
        }

        let benign = [
            (
                "Rename-ItemProperty -Path 'C:\\Temp' -Name Old -NewName New",
                "powershell-registry-change",
            ),
            (
                "Write-Output \"rnp HKLM:\\Software\\Incant Old New\"",
                "powershell-registry-change",
            ),
            ("Get-Partition -DiskNumber 2", "powershell-storage-change"),
            (
                "New-Partition -DiskNumber 2 -WhatIf",
                "powershell-storage-change",
            ),
            (
                "Get-NetFirewallRule -DisplayGroup 'Remote Desktop'",
                "powershell-firewall-change",
            ),
            (
                "Enable-NetFirewallRule -DisplayGroup 'Remote Desktop' -WhatIf",
                "powershell-firewall-change",
            ),
        ];
        for (command, rule) in benign {
            assert_not_rule(command, rule);
        }
    }
    #[test]
    fn powershell_backtick_escapes_are_normalized_only_in_executable_code() {
        for command in [
            "Stop`-Service Spooler",
            "sToP`-sErViCe Spooler",
            "Write-Output \"$(Stop`-Service Spooler)\"",
            "pwsh -Com 'Stop`-Service Spooler'",
        ] {
            assert_flags(command, "powershell-service-change", RiskLevel::Destructive);
        }

        for command in [
            "Write-Output \"Stop`-Service Spooler\"",
            "Write-Output \"`$(Stop`-Service Spooler)\"",
            "Write-Output 'Stop`-Service Spooler'",
            "& 'Stop`-Service' Spooler",
        ] {
            assert_not_rule(command, "powershell-service-change");
        }
    }

    #[test]
    fn powershell_launcher_paths_and_unambiguous_parameters_are_parsed() {
        for command in [
            "pwsh -Com Stop-Service Spooler",
            "PoWeRsHeLl.ExE -COMMAND Stop-Service Spooler",
            "pwsh -Cwa { Stop-Service Spooler } ignored",
            "& 'C:\\Program Files\\PowerShell\\7\\pwsh.exe' -Com 'Stop-Service Spooler'",
            "& \"C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -C Stop-Service Spooler",
        ] {
            assert_flags(command, "powershell-service-change", RiskLevel::Destructive);
        }

        assert_flags(
            "pwsh -Co Stop-Service Spooler",
            "powershell-service-change",
            RiskLevel::Destructive,
        );
        for command in [
            "pwsh -CommandText Stop-Service Spooler",
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Com 'Stop-Service Spooler'",
            "& 'C:\\Tools\\not-pwsh.exe' -Com 'Stop-Service Spooler'",
        ] {
            assert_not_rule(command, "powershell-service-change");
        }
    }

    #[test]
    fn powershell_background_operator_scopes_whatif_without_breaking_call_operator() {
        for command in [
            "Stop-Service Spooler & Get-Service -WhatIf",
            "Get-Date & Stop-Service Spooler",
            "& Stop-Service Spooler",
        ] {
            assert_flags(command, "powershell-service-change", RiskLevel::Destructive);
        }

        for command in [
            "Get-Date & Stop-Service Spooler -WhatIf",
            "Stop-Service Spooler -WhatIf & Get-Service",
            "& Stop-Service Spooler -WhatIf",
            "Write-Output diagnostic 2>&1",
            "Stop-Service Spooler -WhatIf 2>&1",
        ] {
            assert_not_rule(command, "powershell-service-change");
        }
    }

    #[test]
    fn encoded_command_accepts_only_powershell_parameter_prefixes() {
        for parameter in [
            "-e",
            "-ec",
            "-en",
            "-enc",
            "-enco",
            "-encod",
            "-encode",
            "-encoded",
            "-encodedc",
            "-encodedco",
            "-encodedcom",
            "-encodedcomm",
            "-encodedcomma",
            "-encodedcomman",
            "-encodedcommand",
        ] {
            assert_flags(
                &format!("pwsh {parameter} SQBFAFgA"),
                "powershell-encoded-command",
                RiskLevel::Caution,
            );
        }
        assert_flags(
            "& 'C:\\Program Files\\PowerShell\\7\\pwsh.exe' -Enco SQBFAFgA",
            "powershell-encoded-command",
            RiskLevel::Caution,
        );

        for parameter in ["-ed", "-ex", "-encoder", "-encodedCommandText", "-co"] {
            assert_not_rule(
                &format!("pwsh {parameter} SQBFAFgA"),
                "powershell-encoded-command",
            );
        }
        assert_not_rule(
            "Write-Output 'pwsh -EncodedCommand SQBFAFgA'",
            "powershell-encoded-command",
        );
        assert_not_rule(
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Enc SQBFAFgA",
            "powershell-encoded-command",
        );
    }

    #[test]
    fn powershell_here_strings_expand_only_executable_subexpressions() {
        for command in [
            "$text = @'\nStop-Service Spooler; \"quotes\"; $(Stop-Service Spooler)\n'@\nGet-Service Spooler",
            "$text = @\"\nStop-Service Spooler\n\"@\nGet-Service Spooler",
            "@'\nStop-Service Spooler\n'@",
            "$text = @'\nStop-Service Spooler\n  '@\nStop-Service Spooler",
            "$text = @\"\n$(Stop-Service Spooler -WhatIf)\n\"@\nGet-Service Spooler",
        ] {
            assert_not_rule(command, "powershell-service-change");
        }

        for command in [
            "$text = @\"\nStop-Service Spooler | $(Stop-Service Spooler)\n\"@\nGet-Service Spooler",
            "$text = @\"\n$(Stop-Service Spooler)\n\"@ trailing\nGet-Service Spooler",
            "$text = @'\nStop-Service Spooler\n'@\nStop-Service Spooler",
            "$text = @\"\n$(Stop-Service Spooler)\n\"@\r\nStop-Service Spooler",
        ] {
            assert_flags(command, "powershell-service-change", RiskLevel::Destructive);
        }
    }

    #[test]
    fn policy_audit_regressions_cover_all_reported_blockers() {
        let destructive = [
            (
                "$text = @\"\n$(Stop-Service Spooler)\n\"@",
                "powershell-service-change",
            ),
            (
                "Stop-Service -Name $(Write-Output Spooler; New-Item x -ItemType File -WhatIf:$true)",
                "powershell-service-change",
            ),
            ("pwsh -co 'Stop-Service Spooler'", "powershell-service-change"),
            (
                "pwsh -CommandWithArgs 'Stop-Service Spooler'",
                "powershell-service-change",
            ),
            (
                "pwsh -ex Bypass -Command 'Get-Date'",
                "powershell-execution-policy-bypass",
            ),
            (
                "& 'reg.exe' DELETE HKLM\\Software\\Incant /f",
                "windows-registry-native-change",
            ),
            (
                "& 'sc.exe' delete Spooler",
                "windows-service-native-change",
            ),
            (
                "& 'schtasks.exe' /delete /tn Incant /f",
                "windows-schtasks-change",
            ),
            (
                "& 'wevtutil.exe' cl System",
                "windows-event-log-native-clear",
            ),
            (
                "Set-Partition -DiskNumber 2 -PartitionNumber 1 -NewDriveLetter Z",
                "powershell-storage-change",
            ),
        ];
        for (command, rule) in destructive {
            assert_flags(command, rule, RiskLevel::Destructive);
        }

        assert_flags(
            "saps pwsh -Verb RunAs",
            "powershell-runas",
            RiskLevel::Caution,
        );

        for command in [
            "Write-Output foo`;Stop-Service Spooler",
            "$block = { Stop-Service Spooler }",
            "$text = @'\n$(Stop-Service Spooler)\n'@",
        ] {
            assert_not_rule(command, "powershell-service-change");
        }
        assert_flags(
            "& { Stop-Service Spooler }",
            "powershell-service-change",
            RiskLevel::Destructive,
        );
    }

    #[test]
    fn launcher_parameter_prefixes_match_powershell_ambiguity_rules() {
        for parameter in [
            "-c", "-co", "-com", "-comm", "-comma", "-comman", "-command",
        ] {
            assert_flags(
                &format!("pwsh {parameter} 'Stop-Service Spooler'"),
                "powershell-service-change",
                RiskLevel::Destructive,
            );
        }
        for parameter in ["-Cwa", "-CommandWithArgs"] {
            assert_flags(
                &format!("pwsh {parameter} 'Stop-Service Spooler'"),
                "powershell-service-change",
                RiskLevel::Destructive,
            );
        }
        for parameter in [
            "-CommandW",
            "-CommandWi",
            "-CommandWit",
            "-CommandWith",
            "-CommandWithA",
            "-CommandWithAr",
            "-CommandWithArg",
        ] {
            assert_not_rule(
                &format!("pwsh {parameter} 'Stop-Service Spooler'"),
                "powershell-service-change",
            );
        }

        for parameter in [
            "-ep",
            "-ex",
            "-exe",
            "-exec",
            "-execu",
            "-execut",
            "-executi",
            "-executio",
            "-execution",
            "-executionp",
            "-executionpo",
            "-executionpol",
            "-executionpoli",
            "-executionpolic",
            "-executionpolicy",
        ] {
            assert_flags(
                &format!("pwsh {parameter} Bypass -Command Get-Date"),
                "powershell-execution-policy-bypass",
                RiskLevel::Destructive,
            );
        }
    }

    #[test]
    fn quoted_native_executable_paths_are_assessed() {
        let cases = [
            (
                "& 'C:\\Windows\\System32\\reg.exe' DELETE HKLM\\Software\\Incant /f",
                "windows-registry-native-change",
            ),
            (
                "& 'C:\\Windows\\System32\\sc.exe' delete Spooler",
                "windows-service-native-change",
            ),
            (
                "& 'C:\\Windows\\System32\\schtasks.exe' /delete /tn Incant /f",
                "windows-schtasks-change",
            ),
            (
                "& 'C:\\Windows\\System32\\wevtutil.exe' cl System",
                "windows-event-log-native-clear",
            ),
        ];
        for (command, rule) in cases {
            assert_flags(command, rule, RiskLevel::Destructive);
        }
        for command in [
            "Write-Output 'C:\\Windows\\System32\\reg.exe DELETE HKLM\\Software\\Incant /f'",
            "Write-Output C:\\Windows\\System32\\sc.exe delete Spooler",
            "Write-Output 'C:\\Windows\\System32\\schtasks.exe /delete /tn Incant /f'",
            "Write-Output C:\\Windows\\System32\\wevtutil.exe cl System",
        ] {
            assert_eq!(
                assess(command).level,
                RiskLevel::Safe,
                "{command:?} is inert diagnostic text"
            );
        }
    }
}
