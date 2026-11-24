use super::{model::*, store::Store};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn changes(root: &Path, base: Option<&str>) -> (Option<String>, BTreeSet<String>) {
    let head = git(root, &["rev-parse", "--verify", "HEAD"])
        .map(|v| String::from_utf8_lossy(&v).trim().to_owned());
    let prefix = git(root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let prefix = String::from_utf8_lossy(&prefix);
    let mut files = BTreeSet::new();
    if let Some(status) = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
            "--",
            ".",
        ],
    ) {
        for row in status.split(|&b| b == 0).filter(|row| row.len() > 3) {
            let path = String::from_utf8_lossy(&row[3..]);
            if let Some(path) = path.strip_prefix(prefix.trim_end_matches('\n')) {
                files.insert(path.to_owned());
            }
        }
    }
    // Only object IDs are accepted here, never user-supplied Git options or revision syntax.
    if let Some(base) = base.filter(|base| {
        (4..=64).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_hexdigit())
    }) && let Some(diff) = git(
        root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--relative",
            "-z",
            &format!("{base}..HEAD"),
            "--",
            ".",
        ],
    ) {
        files.extend(
            diff.split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned()),
        );
    }
    files.retain(|p| !p.starts_with(".quickagent/blueprint/"));
    (head, files)
}

pub(super) fn verify(
    store: &Store,
    manifest: Option<&Manifest>,
    entities: &[Entity],
    graphs: &[BlueprintGraph],
) -> VerifyReport {
    let Some(manifest) = manifest else {
        return VerifyReport::default();
    };
    let (head_commit, changed) = changes(&store.root, manifest.last_synced_commit.as_deref());
    let mut report = VerifyReport {
        exists: true,
        head_commit,
        last_synced_commit: manifest.last_synced_commit.clone(),
        changed_files: changed.iter().cloned().collect(),
        ..Default::default()
    };
    let mut affected = BTreeSet::new();
    let mut sources = BTreeMap::new();
    for e in entities {
        let mut status = EntityStatus::default();
        let file = e.file_path.as_ref().and_then(|p| {
            sources
                .entry(p)
                .or_insert_with(|| store.source(p).ok().flatten())
                .as_deref()
        });
        let modified = e
            .file_path
            .as_ref()
            .is_some_and(|p| changed.contains(p.strip_prefix("./").unwrap_or(p)));
        if e.file_path.is_some() && file.is_none() {
            status.status = PortStatus::Missing;
        }
        let lines = file
            .map(|s| s.lines().collect::<Vec<_>>())
            .unwrap_or_default();
        for port in &e.ports {
            let anchor = if status.status == PortStatus::Missing && file.is_none() {
                Anchor {
                    status: PortStatus::Missing,
                    suggested_line: None,
                }
            } else if e.file_path.is_none() {
                Anchor {
                    status: PortStatus::Ok,
                    suggested_line: None,
                }
            } else {
                check(port, &lines, modified)
            };
            match anchor.status {
                PortStatus::Ok => report.ok_ports += 1,
                PortStatus::Drifted => {
                    report.drifted_ports += 1;
                    if status.status != PortStatus::Missing {
                        status.status = PortStatus::Drifted;
                    }
                }
                PortStatus::Missing => {
                    report.missing_ports += 1;
                    status.status = PortStatus::Missing;
                }
            }
            status.ports.insert(port.id.clone(), anchor);
        }
        if modified || status.status != PortStatus::Ok {
            affected.insert(e.id.clone());
        }
        report.entities.insert(e.id.clone(), status);
    }
    report.affected_graphs = graphs
        .iter()
        .filter(|g| g.nodes.iter().any(|n| affected.contains(&n.entity_id)))
        .map(|g| g.meta.id.clone())
        .collect();
    report.affected_entities = affected.into_iter().collect();
    report.stale = !report.affected_entities.is_empty()
        || (report.last_synced_commit.is_some()
            && report.head_commit != report.last_synced_commit
            && !report.changed_files.is_empty());
    report
}

fn check(port: &Port, lines: &[&str], modified: bool) -> Anchor {
    let mut anchor = Anchor {
        status: PortStatus::Ok,
        suggested_line: None,
    };
    if let Some(symbol) = port.symbol.as_deref().filter(|s| !s.is_empty()) {
        let matches = |line: &str| {
            line.match_indices(symbol).any(|(i, _)| {
                let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
                (!symbol.chars().next().is_some_and(ident)
                    || !line[..i].chars().next_back().is_some_and(ident))
                    && (!symbol.chars().next_back().is_some_and(ident)
                        || !line[i + symbol.len()..].chars().next().is_some_and(ident))
            })
        };
        if let Some(start) = port.line_start {
            let low = start.saturating_sub(6).min(lines.len());
            let high = port
                .line_end
                .unwrap_or(start)
                .saturating_add(5)
                .min(lines.len());
            if lines
                .get(low..high)
                .is_some_and(|r| r.iter().any(|line| matches(line)))
            {
                return anchor;
            }
        }
        if let Some(i) = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| matches(l))
            .min_by_key(|(i, _)| port.line_start.map_or(*i, |s| (i + 1).abs_diff(s)))
            .map(|(i, _)| i + 1)
        {
            if port.line_start.is_some() {
                anchor.status = PortStatus::Drifted;
            }
            anchor.suggested_line = Some(i);
        } else {
            anchor.status = PortStatus::Missing;
        }
    } else if let Some(start) = port.line_start {
        if start == 0 || port.line_end.unwrap_or(start) > lines.len() {
            anchor.status = PortStatus::Missing;
        } else if modified {
            anchor.status = PortStatus::Drifted;
        }
    }
    anchor
}

#[derive(Serialize)]
pub(super) struct SourceSnippet {
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub lines: Vec<SourceLine>,
}
#[derive(Serialize)]
pub(super) struct SourceLine {
    pub line: usize,
    pub text: String,
}
pub(super) fn read_source_snippet(
    store: &Store,
    file_path: &str,
    start: usize,
    end: usize,
    context: usize,
) -> Result<SourceSnippet> {
    ensure!(
        start > 0 && end >= start && end - start < 300 && context <= 50,
        "invalid source range"
    );
    let source = store.source(file_path)?.context("source file not found")?;
    let from = start.saturating_sub(context).max(1);
    let lines = source
        .lines()
        .enumerate()
        .skip(from - 1)
        .take(end.saturating_add(context) - (from - 1))
        .map(|(i, text)| SourceLine {
            line: i + 1,
            text: text.chars().take(2000).collect(),
        })
        .collect::<Vec<_>>();
    ensure!(
        lines.last().is_some_and(|line| line.line >= start),
        "source start line is outside file"
    );
    Ok(SourceSnippet {
        file_path: file_path.into(),
        line_start: from,
        line_end: lines.last().unwrap().line,
        lines,
    })
}
