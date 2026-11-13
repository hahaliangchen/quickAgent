//! Files the task works on that someone else changed while it runs (design
//! guide §9). The host does not stop on them: before an AI's next request it
//! says which files changed and how, once per version, and the AI decides.
//! The note is appended at the end of the conversation, so everything sent
//! before stays a cached prefix.
use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::{Session, managed_files};

/// The message's name, and the hidden field that records which versions it
/// announced (never sent to the provider).
pub(super) const NAME: &str = "external_changes";
pub(super) const ANNOUNCED: &str = "_external_changes";

/// What this window was already told: (path, version) pairs.
fn announced(messages: &[Value]) -> BTreeSet<(String, String)> {
    messages
        .iter()
        .flat_map(|message| message[ANNOUNCED].as_array().into_iter().flatten())
        .filter_map(|entry| Some((entry["path"].as_str()?.to_owned(), entry["version"].as_str()?.to_owned())))
        .collect()
}

/// Append a note about files that changed outside the task since it last read
/// or wrote them. Skipped while a command runs in the workspace: what a
/// running command changes is settled when it ends, and is not someone else's.
pub(super) async fn announce(s: &Session<'_>, messages: &mut Vec<Value>) {
    if crate::shell_process::active(s.state.workspace.root()).await.is_some() {
        return;
    }
    let (root, task) = (s.state.workspace.root().to_path_buf(), s.task.to_owned());
    let Ok(Ok(changes)) = tokio::task::spawn_blocking(move || managed_files::outside_changes(&root, &task)).await else {
        return;
    };
    let told = announced(messages);
    let new: Vec<_> = changes
        .into_iter()
        .filter(|change| !told.contains(&(change.path.clone(), change.current_hash.clone())))
        .collect();
    if new.is_empty() {
        return;
    }
    let files: Vec<Value> = new.iter().map(|change| json!({"path":change.path,"change":change.change})).collect();
    let content = json!({"host_status":true,"not_a_user_request":true,"files":files,
        "note":"These files changed outside this task since you last read or wrote them, perhaps by the user or another agent. Take the change into account: read a file again before editing it, and keep the other change unless the user says otherwise. If it contradicts what you are doing, ask the user."});
    messages.push(json!({"role":"user","name":NAME,"content":content.to_string(),
        ANNOUNCED: new.iter().map(|change| json!({"path":change.path,"version":change.current_hash})).collect::<Vec<_>>()}));
}
