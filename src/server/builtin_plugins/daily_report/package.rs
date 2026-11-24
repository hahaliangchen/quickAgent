//! The six AI tools of the daily-report plugin. Every tool stays inside the
//! plugin: reading the guide, collecting facts, storing a draft, and one gated
//! submission that the host already forced through an approval card.
use super::{
    activity,
    client::{self, Api},
    store::{Draft, DraftProject, Store},
};
use crate::server::{
    builtin_plugins::ToolHandler,
    plugin_protocol::{NativeContributions, NativeTool, ToolPermission},
    skill_packages::Package,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};


/// The tool name that always asks a human first.
const SUBMIT: &str = "daily_report__submit";

/// The conversation this tool call belongs to, so a prepared draft can bring
/// its own page forward.
#[derive(Clone, Debug, Default)]
struct Scope {
    task_id: Option<String>,
    approved: bool,
}

tokio::task_local! {
    static DAILY_REPORT_SCOPE: Scope;
}
/// Submitting is the only action that leaves the host, and it happens once per
/// approval card.
pub(crate) fn requires_approval(name: &str, _args: &Value) -> bool {
    name == SUBMIT
}

pub(crate) async fn scoped<F: Future>(task_id: String, approved: bool, action: F) -> F::Output {
    DAILY_REPORT_SCOPE
        .scope(
            Scope {
                task_id: Some(task_id),
                approved,
            },
            action,
        )
        .await
}

fn scope() -> Scope {
    DAILY_REPORT_SCOPE.try_with(Clone::clone).unwrap_or_default()
}

fn data_dir() -> Result<PathBuf> {
    Store::data_dir()
}

fn number(args: &Value, key: &str) -> f64 {
    args.get(key).and_then(Value::as_f64).unwrap_or_default()
}

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Read the guide, or say plainly that nobody has written one yet.
fn read_guide(store: &Store) -> Result<Value> {
    let body = store.guide()?;
    Ok(json!({
        "empty": body.trim().is_empty(),
        "content": body,
        "path": store.guide_path().display().to_string(),
        "guide_dir": store.root().display().to_string(),
        "hint": if body.trim().is_empty() {
            "还没有写作规范。向用户确认风格后，用 daily_report.write_guide 写入，或让用户在工作台里直接编辑。"
        } else {
            "按这份规范组织 work_content。"
        },
    }))
}

/// Collect today's facts. Missing sources become warnings, never invented work.
async fn today_activity(workspace: &Path, args: Value) -> Result<Value> {
    let root = workspace.to_path_buf();
    let date = Some(text(&args, "date")).filter(|date| !date.is_empty());
    tokio::task::spawn_blocking(move || activity::today(&root, date.as_deref())).await?
}

/// The store a tool call uses: the same one the page reads.
fn open(dir: &Path) -> Result<Store> {
    Store::open(dir.to_path_buf())
}

async fn list(store: &Store, args: &Value) -> Result<Value> {
    let Some(session) = store.session()? else {
        return Ok(json!({
            "logged_in": false,
            "hint": "还没有登录日报系统。请用户在工作台的「团队日报」页登录后再继续。",
        }));
    };
    let api = Api::new(session)?;
    let projects = api.projects().await?;
    let page_size = args
        .get("page_size")
        .and_then(Value::as_u64)
        .filter(|size| *size > 0 && *size <= 200)
        .unwrap_or(20);
    let mut query = vec![("pageSize", page_size.to_string())];
    for (arg_key, api_key) in [
        ("report_year", "reportYear"),
        ("report_month", "reportMonth"),
        ("report_date", "reportDate"),
    ] {
        let value = text(args, arg_key);
        if !value.is_empty() {
            query.push((api_key, value));
        }
    }
    let reports = api.reports(&query).await?;
    Ok(json!({
        "logged_in": true,
        "projects": projects,
        "recent_reports": reports["rows"],
        "total_reports": reports["total"],
        "hint": "project_name 必须与 projects 里的名字完全一致。",
    }))
}

/// Store a draft the page can review. The event brings that page forward in
/// whatever conversation the call came from.
async fn prepare_draft(store: &Store, workspace: &Path, args: Value) -> Result<Value> {
    let report_date = text(&args, "report_date");
    ensure!(is_day(&report_date), "report_date must be a YYYY-MM-DD date");
    let projects = args["projects"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|project| DraftProject {
            project_id: project["project_id"].as_i64(),
            project_name: project["project_name"].as_str().unwrap_or_default().trim().to_owned(),
            work_content: project["work_content"].as_str().unwrap_or_default().trim().to_owned(),
        })
        .filter(|project| !project.project_name.is_empty() || !project.work_content.is_empty())
        .collect::<Vec<_>>();
    ensure!(!projects.is_empty(), "at least one project with work content is required");
    for project in &projects {
        ensure!(
            !project.project_name.is_empty() && !project.work_content.is_empty(),
            "every project needs both a name and work content"
        );
    }
    let action = text(&args, "action");
    let report_id = text(&args, "report_id");
    let draft = Draft {
        report_date,
        projects,
        problems: text(&args, "problems"),
        remark: text(&args, "remark"),
        overtime_hours: number(&args, "overtime_hours"),
        leave_hours: number(&args, "leave_hours"),
        reimbursement: number(&args, "reimbursement"),
        reimbursement_reason: text(&args, "reimbursement_reason"),
        action: if action == "update" { "update".to_owned() } else { "create".to_owned() },
        report_id: Some(report_id).filter(|id| !id.is_empty()),
        origin: "ai".to_owned(),
        updated_at: client::now(),
    };
    store.save_draft(&draft)?;
    let scope = scope();
    if let Some(task) = scope.task_id.as_deref().filter(|task| !task.is_empty()) {
        let event = json!({
            "draft": draft,
            "guide_dir": store.root().display().to_string(),
        });
        // A missing task row only means there is no page to open.
        let _ = crate::server::agent_service::emit(workspace, task, "daily_report/draft_ready", event).await;
    }
    Ok(json!({
        "stored": true,
        "action": draft.action,
        "report_date": draft.report_date,
        "project_count": draft.projects.len(),
        "hint": "草稿已保存并在工作台打开。请用户确认后再调用 daily_report.submit。",
    }))
}

/// Send the stored draft. The host already showed the approval card.
async fn submit(store: &Store) -> Result<Value> {
    ensure!(
        scope().approved,
        "APPROVAL_REQUIRED: sending a daily report to the company system needs explicit human approval"
    );
    let draft = store.draft()?.context("还没有草稿：先调用 daily_report.prepare_draft")?;
    ensure!(!draft.projects.is_empty(), "draft has no project rows");
    let Some(session) = store.session()? else {
        anyhow::bail!("{}: 未登录日报系统", client::AUTH_EXPIRED);
    };
    let answer = Api::new(session)?.submit(&draft).await?;
    store.clear_draft()?;
    Ok(json!({
        "submitted": true,
        "action": draft.action,
        "report_date": draft.report_date,
        "projects": draft.projects.iter().map(|project| project.project_name.clone()).collect::<Vec<_>>(),
        "answer": answer,
    }))
}

fn is_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 { true } else { byte.is_ascii_digit() }
        })
}

pub(super) async fn execute(dir: &Path, workspace: &Path, name: &str, args: Value) -> Result<Value> {
    let store = open(dir)?;
    Ok(match name {
        "daily_report__read_guide" => read_guide(&store)?,
        "daily_report__write_guide" => {
            let content = args["content"].as_str().unwrap_or_default().to_owned();
            store.write_guide(&content)?;
            json!({
                "saved": true,
                "bytes": content.len(),
                "empty": content.trim().is_empty(),
                "path": store.guide_path().display().to_string(),
            })
        }
        "daily_report__today_activity" => today_activity(workspace, args).await?,
        "daily_report__list" => list(&store, &args).await?,
        "daily_report__prepare_draft" => prepare_draft(&store, workspace, args).await?,
        SUBMIT => submit(&store).await?,
        other => anyhow::bail!("unknown daily report tool: {other}"),
    })
}

/// Who may call what: the guide is the only thing this plugin writes into its
/// own directory besides the draft, so nothing here is a workspace write.
fn permission(name: &str) -> ToolPermission {
    match name {
        "daily_report__write_guide" | SUBMIT => ToolPermission::WorkspaceWrite,
        _ => ToolPermission::ReadOnly,
    }
}

pub(crate) fn package() -> Result<Package> {
    let handler: ToolHandler = Arc::new(|workspace, name, args| {
        Box::pin(async move {
            let directory = data_dir()?;
            let workspace_root = workspace.root().to_path_buf();
            execute(&directory, &workspace_root, &name, args).await
        })
    });
    let definitions: Vec<Value> = serde_json::from_str(include_str!("tools.json"))?;
    let tools = definitions
        .into_iter()
        .map(|definition| -> Result<(String, NativeTool)> {
            let name = definition["name"]
                .as_str()
                .context("daily report tool has no name")?
                .to_owned();
            Ok((
                name.clone(),
                NativeTool {
                    definition,
                    handler: handler.clone(),
                    permission: permission(&name),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let root = std::env::var_os("GROW_RESOURCES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    crate::server::skill_packages::load_builtin(
        &root.join("src/server/builtin_plugins/daily_report"),
        Arc::new(NativeContributions {
            tools,
            frontend_entries: vec!["daily-report.workbench".into()],
            on_unmount: None,
        }),
    )
}

/// Definitions, for the contract test that compares the manifest with the
/// schemas the model actually receives.
#[cfg(test)]
pub(super) fn definitions() -> Vec<Value> {
    serde_json::from_str(include_str!("tools.json")).expect("compiled daily report tool definitions")
}
