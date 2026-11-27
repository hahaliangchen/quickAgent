use super::{model::*, store::Store};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let output = cmd.output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn changes(root: &Path, base: Option<&str>) -> (Option<String>, BTreeSet<String>, bool) {
    let head = git(root, &["rev-parse", "--verify", "HEAD"])
        .map(|v| String::from_utf8_lossy(&v).trim().to_owned());
    let prefix = git(root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let prefix = String::from_utf8_lossy(&prefix);
    let mut files = BTreeSet::new();
    let mut base_commit_missing = false;
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
    }) {
        if let Some(diff) = git(
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
        } else if head.is_some() {
            // Repo exists and has HEAD, but `last_synced_commit` is no longer reachable (e.g. rebase/amend).
            base_commit_missing = true;
        }
    }
    files.retain(|p| !p.starts_with(".quickagent/blueprint/"));
    (head, files, base_commit_missing)
}

fn symbol_in_lines(symbol: &str, lines: &[&str]) -> Option<usize> {
    let matches = |line: &str| {
        line.match_indices(symbol).any(|(i, _)| {
            let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
            (!symbol.chars().next().is_some_and(ident)
                || !line[..i].chars().next_back().is_some_and(ident))
                && (!symbol.chars().next_back().is_some_and(ident)
                    || !line[i + symbol.len()..].chars().next().is_some_and(ident))
        })
    };
    lines
        .iter()
        .enumerate()
        .find(|(_, l)| matches(l))
        .map(|(i, _)| i + 1)
}

pub(super) fn verify(
    store: &Store,
    manifest: Option<&Manifest>,
    entities: &[Entity],
    graphs: &[BlueprintGraph],
    playbooks: &[Playbook],
) -> VerifyReport {
    let Some(manifest) = manifest else {
        return VerifyReport::default();
    };
    let (head_commit, changed, base_commit_missing) =
        changes(&store.root, manifest.last_synced_commit.as_deref());
    let loaded_graph_ids: BTreeSet<&str> = graphs.iter().map(|g| g.meta.id.as_str()).collect();
    let missing_graphs: Vec<String> = manifest
        .graphs
        .iter()
        .filter(|g| !loaded_graph_ids.contains(g.id.as_str()))
        .map(|g| g.id.clone())
        .collect();
    let mut report = VerifyReport {
        exists: true,
        head_commit,
        last_synced_commit: manifest.last_synced_commit.clone(),
        base_commit_missing,
        changed_files: changed.iter().cloned().collect(),
        missing_graphs,
        ..Default::default()
    };
    let mut affected = BTreeSet::new();
    let mut sources = BTreeMap::<String, Option<String>>::new();
    let mut load_source = |path: &str| -> Option<String> {
        let norm = normalize_rel_path(path);
        sources
            .entry(norm.clone())
            .or_insert_with(|| store.source(&norm).ok().flatten())
            .clone()
    };
    for e in entities {
        let mut status = EntityStatus::default();
        let file_norm = e.file_path.as_deref().map(normalize_rel_path);
        let file = file_norm.as_deref().and_then(&mut load_source);
        let mut modified = file_norm
            .as_deref()
            .is_some_and(|norm| changed.contains(norm));
        if file_norm.is_some() && file.is_none() {
            status.status = PortStatus::Missing;
        }
        // Check coarse-grained class properties (anchors: file / path#symbol).
        for prop in &e.properties {
            let anchor = match prop.kind {
                PropertyKind::File => {
                    let norm = normalize_rel_path(&prop.value);
                    let src = load_source(&norm);
                    if src.is_none() {
                        Anchor {
                            status: PortStatus::Missing,
                            suggested_line: None,
                        }
                    } else if changed.contains(&norm) {
                        modified = true;
                        Anchor {
                            status: PortStatus::Drifted,
                            suggested_line: None,
                        }
                    } else {
                        Anchor {
                            status: PortStatus::Ok,
                            suggested_line: None,
                        }
                    }
                }
                PropertyKind::Symbol => {
                    let (target_file, sym) = match prop.value.split_once('#') {
                        Some((f, s)) => (Some(normalize_rel_path(f)), s.trim()),
                        None => (file_norm.clone(), prop.value.trim()),
                    };
                    if let Some(f) = target_file {
                        if let Some(src) = load_source(&f) {
                            let flines: Vec<&str> = src.lines().collect();
                            if let Some(line) = symbol_in_lines(sym, &flines) {
                                if changed.contains(&f) {
                                    modified = true;
                                }
                                Anchor {
                                    status: PortStatus::Ok,
                                    suggested_line: Some(line),
                                }
                            } else {
                                Anchor {
                                    status: PortStatus::Missing,
                                    suggested_line: None,
                                }
                            }
                        } else {
                            Anchor {
                                status: PortStatus::Missing,
                                suggested_line: None,
                            }
                        }
                    } else {
                        Anchor {
                            status: PortStatus::Ok,
                            suggested_line: None,
                        }
                    }
                }
                PropertyKind::Config | PropertyKind::Env | PropertyKind::Rule => Anchor {
                    status: PortStatus::Ok,
                    suggested_line: None,
                },
            };
            match anchor.status {
                PortStatus::Missing => status.status = PortStatus::Missing,
                PortStatus::Drifted if status.status != PortStatus::Missing => {
                    status.status = PortStatus::Drifted;
                }
                _ => {}
            }
            status.properties.insert(prop.id.clone(), anchor);
        }
        status.file_modified = modified;
        let lines = file
            .as_deref()
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
        if modified && status.status == PortStatus::Ok {
            status.status = PortStatus::Drifted;
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
    for pb in playbooks {
        let mut status = PlaybookStatus::default();
        for raw_path in &pb.anchor_files {
            let norm = normalize_rel_path(raw_path);
            let exists = load_source(&norm).is_some();
            if !exists {
                status.missing_files.push(norm);
            } else if changed.contains(&norm) {
                status.changed_files.push(norm);
            }
        }
        if !status.missing_files.is_empty() {
            status.status = PortStatus::Missing;
            report.missing_playbooks += 1;
            report.affected_playbooks.push(pb.id.clone());
        } else if !status.changed_files.is_empty() {
            status.status = PortStatus::Drifted;
            report.drifted_playbooks += 1;
            report.affected_playbooks.push(pb.id.clone());
        } else {
            status.status = PortStatus::Ok;
            report.ok_playbooks += 1;
        }
        report.playbooks.insert(pb.id.clone(), status);
    }
    report.affected_entities = affected.into_iter().collect();
    report.stale = report.base_commit_missing
        || !report.missing_graphs.is_empty()
        || !report.affected_entities.is_empty()
        || !report.affected_playbooks.is_empty()
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
