//! A read-only view of the flow files recorded for one conversation.
use super::program_output_text;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const MAIN_FILES: &str = "
    SELECT seq, json_extract(data,'$.path'), json_extract(data,'$.turn'),
           length(json_extract(data,'$.source'))
    FROM agent_task_events
    WHERE task_id=?1 AND kind='program/start'
      AND json_extract(data,'$.function') IS NULL
      AND json_type(data,'$.path')='text' AND json_type(data,'$.source')='text'
      AND (?2 IS NULL OR json_extract(data,'$.turn')=?2)
      AND (?3 IS NULL OR seq<?3)
    ORDER BY seq";

fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_owned()
}

pub(super) fn is_module_path(path: &str) -> bool {
    normalize(path).starts_with("modules/")
}

fn page(text: &str, args: &Value, key: &str, result: &mut Value) {
    let total = text.chars().count();
    let offset = args["char_offset"].as_u64().unwrap_or(0).min(total as u64) as usize;
    let max = args["max_chars"]
        .as_u64()
        .unwrap_or(12_000)
        .clamp(256, 12_000) as usize;
    let end = offset.saturating_add(max).min(total);
    result[key] = json!(
        text.chars()
            .skip(offset)
            .take(end - offset)
            .collect::<String>()
    );
    result["total_chars"] = json!(total);
    result["next_char_offset"] = json!((end < total).then_some(end));
}

fn source_page(source: &str, args: &Value, mut result: Value) -> Result<Value> {
    let start = args["start_line"].as_u64().unwrap_or(1);
    let end = args["end_line"].as_u64().unwrap_or(u64::MAX);
    ensure!(start > 0 && end >= start, "invalid source line range");
    let total = source.split_inclusive('\n').count();
    let line_start = |line| match line {
        1 => 0,
        line if line > total as u64 => source.len(),
        line => {
            source
                .match_indices('\n')
                .nth((line - 2) as usize)
                .unwrap()
                .0
                + 1
        }
    };
    let selected = &source[line_start(start)..line_start(end.saturating_add(1))];
    result["total_lines"] = json!(total);
    result["start_line"] = json!((start <= total as u64).then_some(start));
    result["end_line"] = json!((start <= total as u64).then_some(end.min(total as u64)));
    page(selected, args, "source", &mut result);
    Ok(result)
}

fn directory_page(directory: &str, entries: impl Iterator<Item = Value>, args: &Value) -> Value {
    let offset = args["directory_offset"].as_u64().unwrap_or(0);
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 12) as usize;
    let mut entries = entries
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(limit + 1)
        .collect::<Vec<_>>();
    let more = entries.len() > limit;
    entries.truncate(limit);
    json!({"directory":directory,"entries":entries,
        "next_directory_offset":more.then(|| offset.saturating_add(limit as u64))})
}

pub(super) fn read(
    conn: &Connection,
    current_task: &str,
    args: &Value,
    boundary: Option<i64>,
) -> Result<Value> {
    let task = args["task_id"].as_str().unwrap_or(current_task);
    let boundary = boundary.filter(|_| task == current_task);
    let turn = args["turn"].as_i64();
    let directory = args["directory"].as_str().unwrap_or(".");
    ensure!(
        matches!(directory, "." | "modules"),
        "flow directories are '.' and 'modules'"
    );
    let wanted = args["program"].as_str().map(normalize);
    if wanted.as_deref().is_some_and(is_module_path) || (wanted.is_none() && directory == "modules")
    {
        return modules(conn, task, turn, boundary, wanted.as_deref(), args);
    }

    let mut stmt = conn.prepare(MAIN_FILES)?;
    let rows = stmt.query_map(params![task, turn, boundary], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let files = rows
        .map(|row| {
            let (seq, path, turn, size) = row?;
            let file = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
            Ok((seq, file, turn, size))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(wanted) = wanted {
        let (seq, path, turn, _) = files
            .iter()
            .rev()
            .find(|(_, path, _, _)| *path == wanted)
            .with_context(|| format!("no flow file {wanted}; list directory '.' first"))?;
        let mut result = json!({"program":path,"turn":turn,"event_seq":seq});
        if args["output"] == true {
            let raw: String = conn
                .query_row(
                    "SELECT data FROM agent_task_events
                 WHERE task_id=?1 AND kind='program/end' AND json_extract(data,'$.program')=?2
                   AND seq>?3 AND (?4 IS NULL OR seq<?4) ORDER BY seq DESC LIMIT 1",
                    params![task, path, seq, boundary],
                    |row| row.get(0),
                )
                .optional()?
                .context("this flow file has no saved output")?;
            let end: Value = serde_json::from_str(&raw)?;
            result["ended"] = end["ended"].clone();
            page(&program_output_text(&end), args, "output", &mut result);
            return Ok(result);
        }
        let source: String = conn.query_row(
            "SELECT json_extract(data,'$.source') FROM agent_task_events WHERE task_id=?1 AND seq=?2",
            params![task, seq], |row| row.get(0),
        )?;
        return source_page(&source, args, result);
    }

    let mut stmt = conn.prepare(
        "SELECT json_extract(data,'$.program'), seq, json_extract(data,'$.ended'),
                json_array_length(data,'$.console')
         FROM agent_task_events WHERE task_id=?1 AND kind='program/end'
           AND json_type(data,'$.program')='text'
           AND (?2 IS NULL OR json_extract(data,'$.turn')=?2)
           AND (?3 IS NULL OR seq<?3) ORDER BY seq",
    )?;
    let ends = stmt
        .query_map(params![task, turn, boundary], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
    let entries = files.into_iter().map(|(seq, path, turn, size)| {
        let end = ends.get(&path).filter(|(end_seq, _, _)| *end_seq > seq);
        json!({"kind":"file","path":path,"turn":turn,"code_chars":size,
            "ended":end.and_then(|(_, status, _)| status.as_deref()),
            "output_lines":end.and_then(|(_, _, lines)| *lines)})
    });
    Ok(directory_page(
        ".",
        std::iter::once(json!({"kind":"directory","path":"modules"})).chain(entries),
        args,
    ))
}

fn modules(
    conn: &Connection,
    task: &str,
    turn: Option<i64>,
    boundary: Option<i64>,
    wanted: Option<&str>,
    args: &Value,
) -> Result<Value> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_modules')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        ensure!(wanted.is_none(), "this conversation has no saved modules");
        return Ok(directory_page("modules", std::iter::empty(), args));
    }
    let version = wanted.and(args["version"].as_i64());
    let mut stmt = conn.prepare(
        "WITH visible AS (
            SELECT name, contract, version, turn, length(source) AS size,
                   row_number() OVER (PARTITION BY contract ORDER BY version DESC) AS latest
            FROM agent_modules AS m WHERE task_id=?1 AND (?2 IS NULL OR turn<=?2)
              AND (?3 IS NULL OR EXISTS (
                  SELECT 1 FROM agent_task_events
                  WHERE task_id=?1 AND kind='module/saved' AND seq<?3
                    AND json_extract(data,'$.contract')=m.contract
                    AND json_extract(data,'$.version')=m.version
              )) AND (?4 IS NULL OR version=?4)
         ) SELECT name, contract, version, turn, size FROM visible WHERE latest=1 ORDER BY name, contract",
    )?;
    let rows = stmt.query_map(params![task, turn, boundary, version], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (name, contract, version, turn, size) = row?;
        let path = format!(
            "modules/{}",
            crate::single_agent::modules::file_name(&name, &contract)
        );
        if let Some(wanted) = wanted {
            if path != wanted {
                continue;
            }
            ensure!(
                args["output"] != true,
                "saved module files contain reusable source; read a main flow file for its output"
            );
            let source: String = conn.query_row(
                "SELECT source FROM agent_modules WHERE task_id=?1 AND contract=?2 AND version=?3",
                params![task, contract, version],
                |row| row.get(0),
            )?;
            return source_page(
                &source,
                args,
                json!({"program":path,"function":name,"turn":turn,"version":version}),
            );
        }
        entries.push(json!({"kind":"file","path":path,"function":name,"version":version,"turn":turn,"code_chars":size}));
    }
    ensure!(
        wanted.is_none(),
        "no matching module file or version; list directory 'modules' first"
    );
    Ok(directory_page("modules", entries.into_iter(), args))
}
