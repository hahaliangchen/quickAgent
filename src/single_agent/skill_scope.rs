//! A skill's instructions follow the code that references it. The AI
//! responsible for a source gets the instructions of the libraries that source
//! requires; a hook gets those its contract names with `@uses`. Nothing passes
//! down calls: a function brings only what it uses, as in a closure.
use super::*;

/// The enabled plugin package whose library has this name.
fn package(s: &Session<'_>, library: &str) -> Option<String> {
    let catalog = s.state.tool_catalog.as_ref()?;
    catalog.packages.catalog().as_array()?.iter().filter(|package| package["enabled"] == true).find_map(|package| {
        let id = package["id"].as_str()?;
        let name = package["library"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| crate::agent_program::libraries::namespace(id));
        (name == library).then(|| id.to_owned())
    })
}
fn project(s: &Session<'_>) -> Option<std::path::PathBuf> {
    crate::project_notes::task_project(s.state.workspace.root(), s.task).ok().flatten()
}
/// A skill a program can require for its instructions alone: an enabled plugin
/// package or a project skill, whether or not it has methods.
pub(super) fn exists(s: &Session<'_>, library: &str) -> bool {
    package(s, library).is_some()
        || project(s).is_some_and(|project| crate::flow_skills::list(&project).iter().any(|skill| skill.name == library))
}
/// The project skills a program can require, by name and purpose.
pub(super) fn project_skills(s: &Session<'_>) -> Vec<(String, String)> {
    project(s)
        .map(|project| crate::flow_skills::list(&project).into_iter().map(|skill| (skill.name, skill.description)).collect())
        .unwrap_or_default()
}
/// The instructions behind a library: the skill they belong to and the text.
fn text(s: &Session<'_>, library: &str) -> Option<(String, String)> {
    let result = if let Some(id) = package(s, library) {
        let read = s.state.tool_catalog.as_ref()?.packages.read(&id).ok()?;
        (id, read)
    } else {
        let read = crate::flow_skills::read(&project(s)?, library)?.ok()?;
        (library.to_owned(), read)
    };
    let content = result.1["content"].as_str().filter(|text| !text.trim().is_empty())?.to_owned();
    Some((result.0, content))
}
/// Skills whose instructions this context already has: those its AI read.
fn loaded(messages: &[Value]) -> BTreeSet<String> {
    messages
        .iter()
        .filter(|m| m[crate::context_assets::REFERENCE]["source_tool"] == "read_skill")
        .filter_map(|m| {
            m[crate::context_assets::REFERENCE]["skill"].as_str().map(str::to_owned).or_else(|| {
                let parsed = serde_json::from_str::<Value>(m["content"].as_str()?).ok()?;
                parsed.pointer("/original_result/skill").or_else(|| parsed.get("skill"))?.as_str().map(str::to_owned)
            })
        })
        .collect()
}
/// The libraries the code this request's AI is responsible for references,
/// and how to say so: the program or module it wrote, the module it fixes, or
/// for a hook what its contract names with @uses.
fn referenced(role: &str, messages: &[Value]) -> (Vec<String>, &'static str) {
    let input = |name: &str| {
        messages
            .iter()
            .find(|m| m["name"] == name)
            .and_then(|m| serde_json::from_str::<Value>(m["content"].as_str()?).ok())
    };
    match role {
        "leaf" => {
            let skills = input("program_task_input")
                .and_then(|task| task["skills"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|skill| skill.as_str().map(str::to_owned))
                .collect();
            (skills, "this function's contract names with @uses")
        }
        "fix" => {
            let source = input(super::modules::FIX_INPUT)
                .and_then(|input| input["module"]["source"].as_str().map(str::to_owned))
                .unwrap_or_default();
            (crate::agent_program::required_libraries(&source), "the module you are fixing requires")
        }
        _ => {
            let source = messages
                .iter()
                .rev()
                .filter(|m| m["role"] == "assistant")
                .find_map(|m| program_flow::written_source(m).and_then(Result::ok))
                .unwrap_or_default();
            let reason = if role == "main" { "your program requires" } else { "your module requires" };
            (crate::agent_program::required_libraries(&source), reason)
        }
    }
}
/// System messages with the instructions of the libraries the code this
/// request's AI is responsible for references, except those it already has.
pub(super) fn instructions(s: &Session<'_>, role: &str, messages: &[Value]) -> Vec<Value> {
    let (libraries, reason) = referenced(role, messages);
    if libraries.is_empty() {
        return Vec::new();
    }
    let mut present = loaded(messages);
    // A plugin page's own instructions already reach its main AI.
    if role == "main"
        && let Some(owner) = &s.plugin
    {
        present.insert(owner.clone());
    }
    libraries
        .into_iter()
        .filter_map(|library| {
            let (skill, content) = text(s, &library)?;
            present.insert(skill).then(|| {
                json!({"role":"system","name":"skill_instructions",
                    "content":format!("Instructions of the {library} library, which {reason}:\n\n{content}")})
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ai_is_given_the_skills_its_own_code_references() {
        let input = |name: &str, content: Value| json!({"role":"user","name":name,"content":content.to_string()});
        let hook = [input("program_task_input", json!({"name":"judge","skills":["commonmark"]}))];
        assert_eq!(referenced("leaf", &hook).0, ["commonmark"], "a hook: what its contract names with @uses");
        let fix = [input(super::super::modules::FIX_INPUT, json!({"module":{"source":"async function check(x) {\n  const md = require(\"markdown/promises\");\n  return md;\n}"}}))];
        assert_eq!(referenced("fix", &fix).0, ["markdown"], "a fixer: what the module it fixes requires");
        let written = |source: &str| super::super::program_flow::fixture_reply(&format!("```js\n{source}\n```"));
        let main = [written("const ppt = require(\"ppt\");"), json!({"role":"user","content":"again"}), written("const flow = require(\"flow\");")];
        assert_eq!(referenced("main", &main).0, ["flow"], "a writer: the latest code it wrote");
        assert!(referenced("implement", &[]).0.is_empty(), "nothing before it writes");
        assert!(referenced("main", &[written("const x = require(")]).0.is_empty(), "code that does not parse references nothing");
    }
}
