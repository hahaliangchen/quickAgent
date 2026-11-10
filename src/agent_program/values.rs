//! Deterministic JSON operations. No model fallback and no JavaScript runtime.
use super::*;
use anyhow::{Result, bail, ensure};

pub(super) fn text(v: &Value) -> Result<String> {
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) => v.to_string(),
        _ => {
            bail!(
                "string concatenation/template/join values must be JSON primitives; use JSON.stringify for containers"
            )
        }
    })
}
const REGEX_PATTERN_LIMIT: usize = 8 * 1024;

// Engines are derived caches; only pattern/flags/lastIndex live in snapshots.
// Ordinary patterns retain regex's linear-time execution. Fancy patterns have
// a fixed backtracking budget, and every consumer propagates exhausted budgets.
pub(super) enum JsRegex {
    Linear(regex::Regex),
    Backtracking(fancy_regex::Regex),
}
#[derive(Clone, Copy)]
pub(super) struct JsMatch<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}
impl<'t> JsMatch<'t> {
    pub(super) fn start(self) -> usize {
        self.start
    }
    pub(super) fn end(self) -> usize {
        self.end
    }
    pub(super) fn as_str(self) -> &'t str {
        &self.text[self.start..self.end]
    }
}
pub(super) struct JsCaptures<'t> {
    groups: Vec<Option<JsMatch<'t>>>,
    names: BTreeMap<String, usize>,
}
impl<'t> JsCaptures<'t> {
    pub(super) fn get(&self, index: usize) -> Option<JsMatch<'t>> {
        self.groups.get(index).copied().flatten()
    }
    pub(super) fn name(&self, name: &str) -> Option<JsMatch<'t>> {
        self.names.get(name).and_then(|i| self.get(*i))
    }
    pub(super) fn len(&self) -> usize {
        self.groups.len()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = Option<JsMatch<'t>>> + '_ {
        self.groups.iter().copied()
    }
}
impl JsRegex {
    pub(super) fn capture_names(&self) -> Box<dyn Iterator<Item = Option<&str>> + '_> {
        match self {
            Self::Linear(re) => Box::new(re.capture_names()),
            Self::Backtracking(re) => Box::new(re.capture_names()),
        }
    }
    fn names(&self) -> BTreeMap<String, usize> {
        self.capture_names()
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| (n.into(), i)))
            .collect()
    }
    pub(super) fn captures_at<'t>(
        &self,
        text: &'t str,
        pos: usize,
    ) -> Result<Option<JsCaptures<'t>>> {
        ensure!(
            text.len() <= 2 * 1024 * 1024,
            "RangeError: regex input exceeds 2 MiB"
        );
        let groups: Option<Vec<Option<JsMatch<'t>>>> = match self {
            Self::Linear(re) => re.captures_at(text, pos).map(|c| {
                c.iter()
                    .map(|m| {
                        m.map(|m| JsMatch {
                            text,
                            start: m.start(),
                            end: m.end(),
                        })
                    })
                    .collect()
            }),
            Self::Backtracking(re) => re
                .captures_from_pos(text, pos)
                .map_err(|e| anyhow::anyhow!("RangeError: regex backtracking failed: {e}"))?
                .map(|c| {
                    c.iter()
                        .map(|m| {
                            m.map(|m| JsMatch {
                                text,
                                start: m.start(),
                                end: m.end(),
                            })
                        })
                        .collect()
                }),
        };
        if let Some(ref groups) = groups {
            ensure!(
                groups
                    .iter()
                    .flatten()
                    .map(|m| m.as_str().len())
                    .sum::<usize>()
                    <= 4 * 1024 * 1024,
                "RangeError: regex capture result exceeds 4 MiB"
            );
        }
        Ok(groups.map(|groups| JsCaptures {
            groups,
            names: self.names(),
        }))
    }
    pub(super) fn captures<'t>(&self, text: &'t str) -> Result<Option<JsCaptures<'t>>> {
        self.captures_at(text, 0)
    }
    pub(super) fn find<'t>(&self, text: &'t str) -> Result<Option<JsMatch<'t>>> {
        Ok(self.captures(text)?.and_then(|c| c.get(0)))
    }
    pub(super) fn is_match(&self, text: &str) -> Result<bool> {
        Ok(self.find(text)?.is_some())
    }
    pub(super) fn captures_iter<'r, 't>(
        &'r self,
        text: &'t str,
    ) -> Box<dyn Iterator<Item = Result<JsCaptures<'t>>> + 'r>
    where
        't: 'r,
    {
        if text.len() > 2 * 1024 * 1024 {
            return Box::new(std::iter::once(Err(anyhow::anyhow!(
                "RangeError: regex input exceeds 2 MiB"
            ))));
        }
        let names = self.names();
        match self {
            Self::Linear(re) => Box::new(re.captures_iter(text).map(move |c| {
                Ok(JsCaptures {
                    groups: c
                        .iter()
                        .map(|m| {
                            m.map(|m| JsMatch {
                                text,
                                start: m.start(),
                                end: m.end(),
                            })
                        })
                        .collect(),
                    names: names.clone(),
                })
            })),
            Self::Backtracking(re) => Box::new(re.captures_iter(text).map(move |c| {
                c.map(|c| JsCaptures {
                    groups: c
                        .iter()
                        .map(|m| {
                            m.map(|m| JsMatch {
                                text,
                                start: m.start(),
                                end: m.end(),
                            })
                        })
                        .collect(),
                    names: names.clone(),
                })
                .map_err(|e| anyhow::anyhow!("RangeError: regex backtracking failed: {e}"))
            })),
        }
    }
}

pub(super) fn compile_regex(pattern: &str, flags: &str) -> Result<JsRegex> {
    ensure!(
        pattern.len() <= REGEX_PATTERN_LIMIT,
        "regex pattern exceeds 8192 bytes"
    );
    let mut seen = String::new();
    for flag in flags.chars() {
        ensure!(
            matches!(flag, 'g' | 'i' | 'm' | 's' | 'u' | 'y'),
            "unsupported regex flag: {flag}"
        );
        ensure!(!seen.contains(flag), "duplicate regex flag: {flag}");
        seen.push(flag);
    }
    let normalized = translate_js_pattern(pattern, flags.contains('s'), flags.contains('u'));
    let mut builder = regex::RegexBuilder::new(&normalized);
    builder
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        // Counted repetition such as [\s\S]{0,1800} is ordinary JS; an
        // automaton unrolls it, so the budget is sized for that, not 1 MB.
        .size_limit(64 * 1024 * 1024);
    match builder.build() {
        Ok(re) => Ok(JsRegex::Linear(re)),
        Err(_) => {
            // Fancy cannot change Unicode mode inline. Spell ASCII word
            // boundaries as lookarounds without introducing capture groups.
            let normalized = normalized
                .replace(
                    r"(?-u:\b)",
                    r"(?:(?<![0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<=[0-9A-Za-z_])(?![0-9A-Za-z_]))",
                )
                .replace(
                    r"(?-u:\B)",
                    r"(?:(?<=[0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<![0-9A-Za-z_])(?![0-9A-Za-z_]))",
                );
            let re = fancy_regex::RegexBuilder::new(&normalized)
                .case_insensitive(flags.contains('i'))
                .multi_line(flags.contains('m'))
                .dot_matches_new_line(flags.contains('s'))
                .backtrack_limit(100_000)
                .build()?;
            Ok(JsRegex::Backtracking(re))
        }
    }
}

/// JavaScript character classes, written as Rust regex syntax.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";
const JS_WORD: &str = "0-9A-Za-z_";

/// Translate a JavaScript pattern to Rust regex syntax with JavaScript's
/// meaning: \d, \w and \b are ASCII; \s is the ECMAScript WhiteSpace and
/// LineTerminator set; `.` excludes all line terminators unless the s flag is
/// set. Inside a class, `[`, `&` and `~` are literal in JavaScript but are
/// nesting and set operators in Rust, so they are escaped.
pub(super) fn translate_js_pattern(pattern: &str, dot_all: bool, unicode: bool) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let chars: Vec<char> = pattern.chars().collect();
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&next) = chars.get(i + 1) else {
                    out.push_str(r"\\");
                    break;
                };
                i += 1;
                match (next, in_class) {
                    ('p' | 'P', _) => {
                        if unicode && chars.get(i + 1) == Some(&'{') {
                            out.push('\\');
                            out.push(next);
                            i += 1;
                            while i < chars.len() {
                                out.push(chars[i]);
                                if chars[i] == '}' {
                                    break;
                                }
                                i += 1;
                            }
                        } else {
                            out.push(next);
                        }
                    }
                    ('/', _) => out.push('/'),
                    ('d', false) => out.push_str("[0-9]"),
                    ('d', true) => out.push_str("0-9"),
                    ('D', _) => out.push_str("[^0-9]"),
                    ('w', false) => out.push_str(&format!("[{JS_WORD}]")),
                    ('w', true) => out.push_str(JS_WORD),
                    ('W', _) => out.push_str(&format!("[^{JS_WORD}]")),
                    ('s', false) => out.push_str(&format!("[{JS_SPACE}]")),
                    ('s', true) => out.push_str(JS_SPACE),
                    ('S', _) => out.push_str(&format!("[^{JS_SPACE}]")),
                    ('b', false) => out.push_str(r"(?-u:\b)"),
                    ('B', false) => out.push_str(r"(?-u:\B)"),
                    ('b', true) => out.push_str(r"\x08"),
                    ('0', _) if !chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) => {
                        out.push_str(r"\x00")
                    }
                    ('c', _) if chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) => {
                        i += 1;
                        out.push_str(&format!(r"\x{:02X}", (chars[i] as u32) % 32));
                    }
                    (other, _) => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' if !in_class => {
                // `[]` matches nothing and `[^]` matches anything in JavaScript.
                if chars.get(i + 1) == Some(&']') {
                    out.push_str(r"[^\x{0}-\x{10FFFF}]");
                    i += 1;
                } else if chars.get(i + 1) == Some(&'^') && chars.get(i + 2) == Some(&']') {
                    out.push_str(r"[\x{0}-\x{10FFFF}]");
                    i += 2;
                } else {
                    in_class = true;
                    out.push('[');
                    if chars.get(i + 1) == Some(&'^') {
                        out.push('^');
                        i += 1;
                    }
                    // A leading `]` is literal in neither language; JavaScript
                    // closed the class above, so nothing else is needed here.
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '[' | '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            '.' if !in_class && !dot_all => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            '{' if !in_class && !is_quantifier(&chars[i..]) => out.push_str(r"\{"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// `{n}`, `{n,}` or `{n,m}` at the start of `rest`.
fn is_quantifier(rest: &[char]) -> bool {
    let mut i = 1;
    let start = i;
    while rest.get(i).is_some_and(char::is_ascii_digit) {
        i += 1;
    }
    if i == start {
        return false;
    }
    if rest.get(i) == Some(&',') {
        i += 1;
        while rest.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
    }
    rest.get(i) == Some(&'}')
}

fn regex_parts(value: &Value) -> Option<(&str, &str)> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let descriptor = object.get("$flow_regex")?.as_object()?;
    Some((
        descriptor.get("pattern")?.as_str()?,
        descriptor.get("flags")?.as_str()?,
    ))
}

pub(super) fn regexp(args: Vec<Value>) -> Result<Value> {
    let original = args.first().and_then(regex_parts);
    let pattern = match args.first() {
        None => String::new(),
        Some(_) if original.is_some() => original.unwrap().0.to_owned(),
        Some(value) => text(value)?,
    };
    let flags = match args.get(1) {
        Some(value) => text(value)?,
        None => original
            .map(|(_, flags)| flags.to_owned())
            .unwrap_or_default(),
    };
    compile_regex(&pattern, &flags)?;
    Ok(json!({"$flow_regex": {"pattern": pattern, "flags": flags}}))
}
