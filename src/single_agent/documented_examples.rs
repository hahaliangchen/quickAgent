//! Execute the published source, not a rewritten approximation of its main.
use super::*;

fn latest_run_marker(name: &str) -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir().join(name)
    } else {
        Path::new("/tmp").join(name)
    }
}

fn missing_local_links(root: &Path, files: &[String]) -> Result<Vec<Value>> {
    let href = regex::Regex::new(r#"(?i)\bhref\s*=\s*[\"']([^\"']+)[\"']"#)?;
    let mut missing = Vec::new();
    for file in files {
        let full = root.join(file);
        let content = std::fs::read_to_string(&full)?;
        for found in href.captures_iter(&content) {
            let link = &found[1];
            let path = link.split(['#', '?']).next().unwrap_or("");
            if path.is_empty() || path.contains(':') || path.starts_with('/') {
                continue;
            }
            if !full.parent().unwrap_or(root).join(path).exists() {
                missing.push(json!({"page":file,"href":link}));
            }
        }
    }
    Ok(missing)
}

#[test]
fn local_navigation_check_reports_missing_files() {
    let root =
        std::env::temp_dir().join(format!("navigation-check-{}", agent_service::uuid_like()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("present.html"), "ok").unwrap();
    std::fs::write(
        root.join("index.html"),
        "<a href='./present.html#part'>ok</a><a href='missing.html'>broken</a>",
    )
    .unwrap();
    let missing = missing_local_links(&root, &["index.html".into()]).unwrap();
    assert_eq!(
        missing,
        vec![json!({"page":"index.html","href":"missing.html"})]
    );
    std::fs::remove_dir_all(root).unwrap();
}



#[tokio::test]
#[ignore = "real Luna TypeScript object-oriented design experiment; consumes provider tokens"]
async fn real_luna_typescript_oop_acceptance_design() -> Result<()> {
    let base=Path::new(env!("CARGO_MANIFEST_DIR"));
    let evidence=base.join("runs").join(format!("luna-ts-oop-five-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&evidence)?;
    std::fs::write(latest_run_marker("luna-ts-oop-five-latest"),evidence.to_string_lossy().as_bytes())?;
    let mut state=agent_service::tests::test_state(&evidence,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    let model=state.model_map.get("gpt-6-luna").cloned().context("configured provider must retain Luna")?;
    let prompt="请为一个已有的五页中文 HTML 项目写 TypeScript 验收脚本：先理解工作区 README.md、docs/agent-program.md、docs/archive/generic-agent-design.md 的项目设计，再从整组页面是否有效介绍项目的角度，结合 runs/acceptance-five-ai-review 下五个 HTML 文件，检查整体和各页问题，并把结论、依据、问题和未检查事项写入 acceptance-report.md。不要修改页面。";
    let instruction="使用传统面向对象方式编写 TypeScript：用类组织职责、状态和流程。能用普通代码与现有工具可靠实现的步骤应写出具体实现；需要理解、判断或其他尚无法直接编程实现的能力，声明为职责内聚的抽象方法，清楚定义参数和返回类型，不必编写这些抽象方法的实现类。宿主可提供满足抽象类接口的实例。文件系统能力也可以作为接口声明。适度设计类，避免将整个流程变成一个抽象方法；检查基于真实内容和证据，准确记录无法确认的内容。请只返回完整 TypeScript 源码，不要解释或 Markdown 围栏。无需为当前特定虚拟机限制代码。";
    std::fs::write(evidence.join("request.json"),serde_json::to_vec_pretty(&json!({"model":model,"system":instruction,"user":prompt}))?)?;
    eprintln!("TS_OOP_START {} model={model}",evidence.display());
    let started=std::time::Instant::now();
    let response=state.model_client.post(format!("{}/chat/completions",state.provider_url.trim_end_matches('/'))).bearer_auth(&state.api_key).json(&json!({"model":model,"messages":[{"role":"system","content":instruction},{"role":"user","content":prompt}],"stream":false})).send().await?.error_for_status()?;
    let result:Value=response.json().await?;
    let raw=result["choices"][0]["message"]["content"].as_str().context("model returned no source")?.trim();
    let source=if raw.starts_with("```"){raw.split_once('\n').map(|(_,s)|s).unwrap_or(raw).trim_end_matches('`').trim()}else{raw};
    std::fs::write(evidence.join("acceptance.ts"),source)?;
    let map:swc_common::sync::Lrc<swc_common::SourceMap>=Default::default();
    let file=map.new_source_file(swc_common::FileName::Custom("acceptance.ts".into()).into(),source.to_owned());
    let lexer=swc_ecma_parser::lexer::Lexer::new(swc_ecma_parser::Syntax::Typescript(swc_ecma_parser::TsSyntax::default()),Default::default(),swc_ecma_parser::StringInput::from(&*file),None);
    let mut parser=swc_ecma_parser::Parser::new_from(lexer);
    let parsed=parser.parse_typescript_module();
    let errors=parser.take_errors();
    let syntax_ok=parsed.is_ok() && errors.is_empty();
    let report=json!({"model":model,"seconds":started.elapsed().as_secs_f64(),"usage":result["usage"],"typescript_syntax_ok":syntax_ok,"syntax_error":parsed.err().map(|e|format!("{:?}",e.kind())),"parser_errors":errors.iter().map(|e|format!("{:?}",e.kind())).collect::<Vec<_>>(),"executed":false});
    std::fs::write(evidence.join("report.json"),serde_json::to_vec_pretty(&report)?)?;
    eprintln!("TS_OOP_REPORT {report}");
    Ok(())
}

#[tokio::test]
#[ignore = "real Luna minimal user-only TypeScript acceptance experiment"]
async fn real_luna_typescript_minimal_prompt() -> Result<()> {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("runs").join(format!("luna-html-js-single-responsibility-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&root)?;
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    let model=state.model_map.get("gpt-6-luna").cloned().context("configured provider must retain Luna")?;
    let prompt="请用 JavaScript 写一份过程式代码，验收我已有的五页 HTML 项目。项目根目录是 /home/goodtime/Documents/project/codex-workspace-mcp，五个页面都在 runs/acceptance-five-ai-review 目录下，分别是 01-overview.html、02-program.html、03-tools.html、04-agents.html、05-quality.html。项目设计资料是根目录下的 README.md、docs/agent-program.md 和 docs/archive/generic-agent-design.md。

请先根据设计资料理解项目目标，再检查五页作为一组是否完整、内容是否与设计一致、各页之间是否协调；同时检查页面本地链接与资源是否有效。根据实际检查结果写一份验收报告，包含整体和逐页结论、具体问题、依据以及没有检查或无法确认的事项。不要修改已有页面。

用普通代码组织完整流程，并实现可以用代码和工具可靠完成的步骤。主程序需要 AI 的能力用带 @module 注释的空函数声明，写清职责、参数和返回类型。模块的实现再用 @virtual 声明职责单一的 AI 钩子。

按单一职责原则设计函数，保持高内聚、低耦合。每个虚函数尽量只做一件事，不要让一个虚函数承担过多不同职责；普通函数负责组合调用和具体执行。按实际需要划分，不为形式机械拆分。";
    std::fs::write(root.join("prompt.txt"),prompt)?;
    eprintln!("TS_MINIMAL_START {} model={model}",root.display());
    let started=std::time::Instant::now();
    let response=state.model_client.post(format!("{}/chat/completions",state.provider_url.trim_end_matches('/'))).bearer_auth(&state.api_key).json(&json!({"model":model,"messages":[{"role":"user","content":prompt}],"stream":false})).send().await?.error_for_status()?;
    let result:Value=response.json().await?;
    let raw=result["choices"][0]["message"]["content"].as_str().context("model returned no output")?;
    std::fs::write(root.join("response.md"),raw)?;
    let source=raw.split("```javascript").nth(1).or_else(||raw.split("```js").nth(1)).or_else(||raw.split("```typescript").nth(1)).or_else(||raw.split("```ts").nth(1)).map(|s|s.split("```").next().unwrap_or(s));
    let mut syntax_ok=None;
    if let Some(source)=source {
        std::fs::write(root.join("acceptance.js"),source.trim())?;
        let map:swc_common::sync::Lrc<swc_common::SourceMap>=Default::default();
        let file=map.new_source_file(swc_common::FileName::Custom("acceptance.ts".into()).into(),source.to_owned());
        let lexer=swc_ecma_parser::lexer::Lexer::new(swc_ecma_parser::Syntax::Typescript(swc_ecma_parser::TsSyntax::default()),Default::default(),swc_ecma_parser::StringInput::from(&*file),None);
        let mut parser=swc_ecma_parser::Parser::new_from(lexer);
        syntax_ok=Some(parser.parse_typescript_module().is_ok() && parser.take_errors().is_empty());
    }
    let report=json!({"model":model,"seconds":started.elapsed().as_secs_f64(),"usage":result["usage"],"user_messages":1,"system_messages":0,"tool_declarations":0,"typescript_syntax_ok":syntax_ok,"executed":false});
    std::fs::write(root.join("report.json"),serde_json::to_vec_pretty(&report)?)?;
    eprintln!("TS_MINIMAL_REPORT {report}");
    Ok(())
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_async_five_pages_published_example() -> Result<()> {
    let root=std::env::temp_dir().join(format!("async-five-real-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(root.join("docs"))?;
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/agent-design-guide.md"),root.join("docs/agent-design-guide.md"))?;
    let source=include_str!("../../skills/task-program/examples/parallel-five-pages.js");
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    let model=state.model_map.get(&state.default_model).cloned().unwrap_or_else(||state.default_model.clone());
    ensure!(!state.provider_url.is_empty() && !state.api_key.is_empty(),"configured model unavailable");
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','',?1,'running',0,0)",[&model])?;
    eprintln!("REAL_ASYNC_FIVE_START model={model} root={}",root.display());
    let prompt=format!("Run exactly this program: call write_program with it unchanged as the source.\n```js\n{}\n```",source.trim());
    let cancel=CancellationToken::new();
    let result=tokio::time::timeout(Duration::from_secs(600),run_task(state,"example".into(),model,prompt,80,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();}
    result.context("real async example timed out")??;
    for page in super::program_flow::example_pages(source) {
        let file=root.join(page);
        ensure!(file.is_file() && std::fs::metadata(&file)?.len()>0,"missing page: {}",file.display());
    }
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0))?;
    ensure!(status=="completed","real async example status: {status}");
    let calls:i64=conn.query_row("SELECT count(*) FROM agent_request_contexts WHERE task_id='example'",[],|row|row.get(0))?;
    eprintln!("REAL_ASYNC_FIVE_COMPLETE model_requests={calls} root={}",root.display());
    Ok(())
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_serial_published_example() -> Result<()> {
    let root=std::env::temp_dir().join(format!("serial-real-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&root)?;
    let source=include_str!("../../skills/task-program/examples/serial.js");
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    let model=state.model_map.get(&state.default_model).cloned().unwrap_or_else(||state.default_model.clone());
    ensure!(!state.provider_url.is_empty() && !state.api_key.is_empty(),"configured model unavailable");
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','',?1,'running',0,0)",[&model])?;
    eprintln!("REAL_SERIAL_START model={model} root={}",root.display());
    // The production path: the main AI replies with the program, and every
    // virtual function in it is then implemented by the real model.
    let prompt=format!("Run exactly this program: call write_program with it unchanged as the source.\n```js\n{}\n```",source.trim());
    let cancel=CancellationToken::new();
    let result=tokio::time::timeout(Duration::from_secs(480),run_task(state,"example".into(),model,prompt,40,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();}
    result.context("real serial example timed out")??;
    let file=root.join("agent-overview.html");
    ensure!(file.is_file() && std::fs::metadata(&file)?.len()>0,"missing output: {}",file.display());
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0))?;
    ensure!(status=="completed","real serial example status: {status}");
    eprintln!("REAL_SERIAL_COMPLETE root={}",root.display());
    Ok(())
}

/// The configured default model at low reasoning effort, in a fresh workspace.
fn real_low_effort(name: &str) -> Result<(std::path::PathBuf, AgentServiceState, String)> {
    real_with_effort(name, "low")
}

/// The reasoning effort a real-model test runs at: REAL_EFFORT when set,
/// otherwise the test's own.
fn real_effort(default: &str) -> String {
    std::env::var("REAL_EFFORT").ok().filter(|effort| !effort.is_empty()).unwrap_or_else(|| default.into())
}

/// REAL_WIRE=responses speaks to the configured provider over the Responses
/// API, as if its config said `api: openai-responses`.
fn real_wire(state: &mut AgentServiceState) {
    if std::env::var("REAL_WIRE").as_deref() == Ok("responses")
        && let Some(route) = state.provider_routes.get_mut(&state.provider_name)
    {
        route.responses = true;
    }
}

/// The configured default model at the given reasoning effort, in a fresh
/// workspace. REAL_PROVIDER names another configured provider, and REAL_MODEL
/// another model of the provider in use.
fn real_with_effort(name: &str, effort: &str) -> Result<(std::path::PathBuf, AgentServiceState, String)> {
    let root=std::env::temp_dir().join(format!("{name}-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&root)?;
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    if let Ok(provider)=std::env::var("REAL_PROVIDER") && !provider.is_empty() {
        ensure!(state.use_provider(&provider),"provider {provider} is not configured");
    }
    state.reasoning_effort=Some(real_effort(effort));
    let model=std::env::var("REAL_MODEL").ok().filter(|model|!model.is_empty())
        .unwrap_or_else(||state.model_map.get(&state.default_model).cloned().unwrap_or_else(||state.default_model.clone()));
    // A local service may need no key.
    ensure!(!state.provider_url.is_empty() && (!state.api_key.is_empty() || std::env::var("REAL_PROVIDER").is_ok()),"configured model unavailable");
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','',?1,'running',0,0)",[&model])?;
    Ok((root,state,model))
}

fn program_outcomes(root: &Path) -> Result<Vec<String>> {
    let conn=agent_service::open_db(root)?;
    let rows=conn.prepare("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE task_id='example' AND kind='assistant/message' AND json_extract(data,'$.actor')='program' ORDER BY seq")?
        .query_map([],|row|row.get::<_,Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

fn last_answer(root: &Path) -> Result<String> {
    Ok(agent_service::open_db(root)?.query_row("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE task_id='example' AND kind='assistant/message' ORDER BY seq DESC LIMIT 1",[],|row|row.get(0))?)
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_leaf_asks_and_the_answer_continues() -> Result<()> {
    let (root,state,model)=real_low_effort("ask-real")?;
    eprintln!("REAL_ASK_START model={model} root={}",root.display());
    let source="import * as io from \"host:io\";\nconst page = await writeIntro(\"an agent runtime that runs AI-written programs\");\nio.write({path: \"intro.html\", content: page.html});\n\n/** @module */\nasync function writeIntro(subject) {\n  // Write a short standalone HTML introduction to subject. Its audience, developers or managers, is not known and changes the whole page: call a @virtual helper that asks the user which audience before writing anything. Returns {html:string}.\n}";
    let prompt=format!("Run exactly this program: call write_program with it unchanged as the source.\n```js\n{source}\n```");
    let started=std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(480),run_task(state.clone(),"example".into(),model.clone(),prompt,40,CancellationToken::new(),1,false)).await.context("first turn timed out")??;
    let first=program_outcomes(&root)?;
    eprintln!("REAL_ASK_FIRST seconds={:.0} outcomes={first:?}",started.elapsed().as_secs_f64());
    ensure!(first.last().is_some_and(|text| text.contains("等你回答")),"the leaf did not ask: {first:?}");
    ensure!(!root.join("intro.html").exists(),"the page was written before the answer");
    tokio::time::timeout(Duration::from_secs(480),run_task(state,"example".into(),model,"Developers.".into(),40,CancellationToken::new(),2,false)).await.context("second turn timed out")??;
    eprintln!("REAL_ASK_SECOND seconds={:.0} outcomes={:?} answer={:?}",started.elapsed().as_secs_f64(),program_outcomes(&root)?,last_answer(&root)?);
    ensure!(root.join("intro.html").is_file(),"the answer did not lead to the page");
    Ok(())
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_user_message_pauses_the_program() -> Result<()> {
    let (root,state,model)=real_low_effort("interject-real")?;
    eprintln!("REAL_INTERJECT_START model={model} root={}",root.display());
    let source="import * as io from \"host:io\";\nconst topics = [\"agents\", \"programs\", \"tools\"];\nfor (const topic of topics) {\n  const page = await writePage(topic);\n  io.write({path: topic + \".html\", content: page.html});\n}\n\n/** @module */\nasync function writePage(topic) {\n  // Use a focused @virtual helper to draft a short standalone HTML page, under 40 lines, explaining topic in an agent runtime. Returns {html:string}.\n}";
    let prompt=format!("Run exactly this program: call write_program with it unchanged as the source.\n```js\n{source}\n```");
    // The user speaks once the first page is written.
    let watched=root.clone();
    let speaker=tokio::spawn(async move {
        while !watched.join("agents.html").exists() { tokio::time::sleep(Duration::from_millis(200)).await; }
        crate::interjections::submit(&watched,"example","Also add a page faq.html answering three common questions about the runtime.").map(|_|())
    });
    let started=std::time::Instant::now();
    let result=tokio::time::timeout(Duration::from_secs(600),run_task(state,"example".into(),model,prompt,60,CancellationToken::new(),1,false)).await;
    speaker.abort();
    result.context("turn timed out")??;
    let files=["agents.html","programs.html","tools.html","faq.html"].into_iter().filter(|file|root.join(file).is_file()).collect::<Vec<_>>();
    let outcomes=program_outcomes(&root)?;
    eprintln!("REAL_INTERJECT_DONE seconds={:.0} files={files:?} outcomes={outcomes:?} answer={:?}",started.elapsed().as_secs_f64(),last_answer(&root)?);
    ensure!(outcomes.iter().any(|text| text.contains("被你的新消息暂停")),"the program was not paused: {outcomes:?}");
    ensure!(root.join("faq.html").is_file(),"the main AI did not handle the message");
    Ok(())
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_makes_a_five_slide_deck_on_its_own() -> Result<()> {
    real_deck(5, 1200).await
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_makes_a_ten_slide_deck_on_its_own() -> Result<()> {
    real_deck(10, 1800).await
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_makes_a_twenty_slide_deck_on_its_own() -> Result<()> {
    real_deck(20, 2400).await
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_makes_a_fifty_slide_deck_on_its_own() -> Result<()> {
    real_deck(50, 3600).await
}

/// What a user would type, with no program: a deck of separate linked pages.
async fn real_deck(count: usize, seconds: u64) -> Result<()> {
    let (root,state,model)=real_low_effort(&format!("deck{count}-real"))?;
    std::fs::create_dir_all(root.join("docs"))?;
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/agent-design-guide.md"),root.join("docs/agent-design-guide.md"))?;
    eprintln!("REAL_DECK_START pages={count} model={model} root={}",root.display());
    let prompt=format!("根据 docs/agent-design-guide.md，做 {count} 个单独的 HTML 文件，每个文件一页，把这个 Agent 的设计完整讲清楚，PPT 风格。页面之间能前后翻页（按钮和左右方向键都行）；{count} 页版式统一；幻灯片在窗口里居中，窗口大小变化时内容不重叠、不被遮挡。");
    let started=std::time::Instant::now();
    let cancel=CancellationToken::new();
    let result=tokio::time::timeout(Duration::from_secs(seconds),run_task(state,"example".into(),model,prompt,400,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();}
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0))?;
    let requests:i64=conn.query_row("SELECT count(*) FROM agent_request_contexts WHERE task_id='example'",[],|row|row.get(0))?;
    let functions:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='program/start' AND json_extract(data,'$.function') IS NOT NULL",[],|row|row.get(0))?;
    let heals:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='program/runtime_error'",[],|row|row.get(0))?;
    let pages=walkdir_html(&root)?;
    eprintln!("REAL_DECK_DONE seconds={:.0} status={status} model_requests={requests} implemented_functions={functions} runtime_errors={heals} pages={} files={pages:?} answer={:?}",
        started.elapsed().as_secs_f64(),pages.len(),last_answer(&root).ok());
    result.context("deck task timed out")??;
    ensure!(status=="completed","deck task status: {status}");
    ensure!(pages.len()>=count,"expected {count} page files: {pages:?}");
    // Each page links to another one.
    for page in &pages {
        let html=std::fs::read_to_string(root.join(page))?;
        ensure!(pages.iter().filter(|other|*other!=page).any(|other|html.contains(Path::new(other).file_name().and_then(|n|n.to_str()).unwrap_or("\0"))),"{page} links to no other page");
    }
    Ok(())
}

fn walkdir_html(root: &Path) -> Result<Vec<String>> {
    let mut found=Vec::new();
    let mut dirs=vec![root.to_owned()];
    while let Some(dir)=dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path=entry?.path();
            let name=path.file_name().and_then(|n|n.to_str()).unwrap_or("");
            if name.starts_with('.') { continue; }
            if path.is_dir() { dirs.push(path); }
            else if name.ends_with(".html") { found.push(path.strip_prefix(root)?.display().to_string()); }
        }
    }
    found.sort();
    Ok(found)
}

/// What group B of docs/skill-experiment-plan.md is given.
const CODING_SKILLS: [&str; 2] = ["spec-driven-testing", "commonmark"];

/// Make repository skills project skills of a test workspace, where
/// list_skills and read_skill find them as in a user's project.
pub(super) fn install_skills(root: &Path, names: &[&str]) -> Result<()> {
    for name in names {
        let source=Path::new(env!("CARGO_MANIFEST_DIR")).join("skills").join(name);
        let target=root.join(crate::flow_skills::DIR).join(name);
        std::fs::create_dir_all(&target)?;
        for entry in std::fs::read_dir(&source)? {
            let entry=entry?;
            std::fs::copy(entry.path(),target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_writes_a_markdown_library_on_its_own() -> Result<()> {
    real_markdown_library(&[]).await
}

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_writes_a_markdown_library_with_skills() -> Result<()> {
    real_markdown_library(&CODING_SKILLS).await
}

async fn real_markdown_library(skills: &[&str]) -> Result<()> {
    let (root,state,model)=real_with_effort("mdlite-real","medium")?;
    install_skills(&root,skills)?;
    eprintln!("REAL_MDLITE_START model={model} skills={skills:?} root={}",root.display());
    // A real coding task: the agent writes and tests the library itself; the
    // hidden acceptance cases are applied only afterwards.
    let prompt="在当前目录用 Python 写一个把 Markdown 转成 HTML 的库：模块 mdlite.py，提供函数 markdown_to_html(text: str) -> str，只用标准库。支持标题、段落、强调（* _ ** __）、行内代码、链接、无序和有序列表（含嵌套）、围栏代码块、引用；HTML 特殊字符要转义；输出按 CommonMark 的 HTML 写法。写单元测试并确保通过。";
    let started=std::time::Instant::now();
    let cancel=CancellationToken::new();
    let result=tokio::time::timeout(Duration::from_secs(1800),run_task(state,"example".into(),model,prompt.into(),300,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();}
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0))?;
    let requests:i64=conn.query_row("SELECT count(*) FROM agent_request_contexts WHERE task_id='example'",[],|row|row.get(0))?;
    let heals:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='program/runtime_error'",[],|row|row.get(0))?;
    let hidden=std::env::temp_dir().join(format!("mdlite-acceptance-{}.py",agent_service::uuid_like()));
    std::fs::write(&hidden,include_str!("fixtures/mdlite_acceptance.py"))?;
    let scored=std::process::Command::new("python3").arg("-I").arg(&hidden).arg(&root).output()?;
    std::fs::remove_file(&hidden)?;
    let score=String::from_utf8_lossy(&scored.stdout).trim().to_owned();
    eprintln!("REAL_MDLITE_DONE seconds={:.0} status={status} model_requests={requests} runtime_errors={heals} files={:?} answer={:?}",
        started.elapsed().as_secs_f64(),walk_files(&root)?,last_answer(&root).ok());
    eprintln!("REAL_MDLITE_SCORE {score} {}",String::from_utf8_lossy(&scored.stderr).trim());
    // Whether it used the skills: what it loaded, and whether its calls or
    // the files it wrote reach the examples.
    let loaded=conn.prepare("SELECT json_extract(data,'$.arguments') FROM agent_task_events WHERE task_id='example' AND kind='tool/call' AND json_extract(data,'$.name')='read_skill' ORDER BY seq")?
        .query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let example_calls:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='tool/call' AND data LIKE '%examples.json%'",[],|row|row.get(0))?;
    let citing=walk_files(&root)?.into_iter().filter(|file|std::fs::read_to_string(root.join(file)).is_ok_and(|text|text.contains("examples.json"))).collect::<Vec<_>>();
    eprintln!("REAL_MDLITE_SKILLS read_skill={loaded:?} example_calls={example_calls} files_citing_examples={citing:?}");
    // How it used modules: the ones written, fixes tried and saved, calls that
    // ran saved code, and calls of @virtual functions.
    let count=|sql:&str|conn.query_row(sql,[],|row|row.get::<_,i64>(0)).unwrap_or(0);
    eprintln!("REAL_MDLITE_MODULES written={} fix_attempts={} fixed_versions={} loaded={} virtual_calls={}",
        count("SELECT count(DISTINCT contract) FROM agent_modules WHERE task_id='example'"),
        count("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='module/fix_started'"),
        count("SELECT count(*) FROM agent_modules WHERE task_id='example' AND fixed=1"),
        count("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='module/loaded'"),
        count("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind='task/node_planned' AND json_extract(data,'$.inputs.program_task.role')='leaf'"));
    result.context("markdown task timed out")??;
    ensure!(status=="completed","markdown task status: {status}");
    let score:Value=serde_json::from_str(&score).context("hidden acceptance did not run")?;
    let (passed,total)=(score["passed"].as_u64().unwrap_or(0),score["total"].as_u64().unwrap_or(1));
    ensure!(passed*10>=total*8,"passed {passed} of {total} hidden cases");
    Ok(())
}

fn walk_files(root: &Path) -> Result<Vec<String>> {
    let mut found=Vec::new();
    let mut dirs=vec![root.to_owned()];
    while let Some(dir)=dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path=entry?.path();
            let name=path.file_name().and_then(|n|n.to_str()).unwrap_or("");
            if name.starts_with('.') || name=="__pycache__" { continue; }
            if path.is_dir() { dirs.push(path); } else { found.push(path.strip_prefix(root)?.display().to_string()); }
        }
    }
    found.sort();
    Ok(found)
}

#[test]
fn commonmark_examples_hold_no_hidden_case() {
    let fixture=include_str!("fixtures/mdlite_acceptance.py");
    // \x5c is a backslash: the strings are Python literals with escapes.
    let case=regex::Regex::new(r#"(?m)^\s*\("(?:[^"\x5c]|\x5c.)*", "((?:[^"\x5c]|\x5c.)*)", \["#).unwrap();
    let hidden=case.captures_iter(fixture).map(|found|serde_json::from_str::<String>(&format!("\"{}\"",&found[1])).unwrap()).collect::<Vec<_>>();
    assert_eq!(hidden.len(),fixture.lines().filter(|line|line.trim_start().starts_with("(\"")).count(),"every hidden case is read");
    let examples:Value=serde_json::from_str(include_str!("../../skills/commonmark/examples.json")).unwrap();
    for example in examples.as_array().unwrap() {
        let markdown=example["markdown"].as_str().unwrap();
        assert!(!hidden.iter().any(|input|input==markdown.trim_end_matches('\n')),"examples.json holds the hidden case {markdown:?}");
    }
}

/// Group B's wiring without a real model: the skills are advertised, found
/// by list_skills and read_skill, and what was read reaches the functions the
/// program calls.
#[tokio::test]
async fn skills_reach_only_the_code_that_references_them() {
    use axum::{Json, Router, routing::post};
    let root=std::env::temp_dir().join(format!("skills-wiring-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&root).unwrap();
    install_skills(&root,&CODING_SKILLS).unwrap();
    let seen=std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let log=seen.clone();
    let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {
        log.lock().unwrap().push(body.clone());
        let messages=body["messages"].as_array().unwrap();
        let all=body["messages"].to_string();
        let results=messages.iter().filter(|m|m["role"]=="tool").count();
        let call=|name:&str,args:Value| json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
            "tool_calls":[{"id":format!("call-{results}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]});
        let text=|content:&str| program_flow::fixture_completion(content);
        let function=messages.iter().find(|m|m["name"]=="program_task_input")
            .map(|m|serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()["name"].as_str().unwrap().to_owned());
        let answer=match function.as_deref() {
            Some("judge")=>text(r#"{"ok":true}"#),
            Some("count")=>text(r#"{"n":1}"#),
            // The module's code requires a skill; its first version does not compile.
            Some(_) if all.contains("PROGRAM_SOURCE_INVALID")=>text("```js\nasync function check(input) {\n  require(\"spec-driven-testing\");\n  return {ok: true};\n}\n```"),
            Some(_)=>text("```js\nimport * as io from \"host:io\";\nasync function check(input) {\n  require(\"spec-driven-testing\");\n  io.nothing({});\n  return {ok: true};\n}\n```"),
            None if all.contains("PROGRAM_RUNTIME_ERROR")=>text("```js\nconst cm = require(\"commonmark\");\nconst kept = await judge(\"x\");\nconst counted = await count(\"x\");\nconst checked = await check(\"x\");\n\n/**\n * @virtual\n * Decide whether x follows the rules.\n * @uses cm the rules to follow\n * Returns {ok:boolean}.\n */\nasync function judge(x) {}\n\n/**\n * @virtual\n * Count the characters of x.\n * Returns {n:number}.\n */\nasync function count(x) {}\n\n/** @module */\nasync function check(input) {\n  // Return {ok:true}.\n}\n```"),
            None=>match results {
                0=>call("list_skills",json!({})),
                1=>call("read_skill",json!({"name":"commonmark"})),
                // It requires a skill it has not read; the program fails once.
                _=>text("```js\nconst cm = require(\"commonmark\");\nconst spec = require(\"spec-driven-testing\");\nthrow new Error(\"first try\");\n```"),
            },
        };
        async move { Json(answer) }
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state=agent_service::tests::test_state(&root,listener.local_addr().unwrap());
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap() });
    agent_service::open_db(&root).unwrap().execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','','fake','running',0,0)",[]).unwrap();
    run_task(state,"example".into(),"fake".into(),"Write a Markdown library".into(),20,CancellationToken::new(),1,false).await.unwrap();
    server.abort();
    let seen=seen.lock().unwrap().clone();
    let named=|body:&Value,name:&str| body["messages"].as_array().unwrap().iter().filter(|m|m["name"]==name).map(|m|m["content"].as_str().unwrap_or("").to_owned()).collect::<Vec<_>>().join("\n");
    let function=|body:&Value| body["messages"].as_array().unwrap().iter().find(|m|m["name"]=="program_task_input")
        .map(|m|serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()["name"].as_str().unwrap().to_owned());
    let of=|name:&str| seen.iter().filter(|body|function(body).as_deref()==Some(name)).cloned().collect::<Vec<_>>();
    let main=seen.iter().filter(|body|function(body).is_none()).cloned().collect::<Vec<_>>();
    // commonmark's instructions hold the delimiter stack; spec-driven-testing's say to write tests from the spec.
    let (commonmark,testing)=("分隔符栈","按规格写测试");
    assert_eq!(main.len(),4,"three requests before the program and one to heal it");
    for body in &main {
        assert!(named(body,"project_skills").contains("- commonmark:"),"the main AI is told which skills it can read");
    }
    assert!(main[2].to_string().contains(commonmark),"read_skill returns the instructions");
    let healing=named(&main[3],"skill_instructions");
    assert!(healing.contains(testing) && healing.contains("your program requires"),"the program requires a skill its AI had not read: {healing}");
    assert!(!healing.contains(commonmark),"what the AI already read is not repeated");
    let judge=of("judge");
    assert_eq!(judge.len(),1);
    let named_skill=named(&judge[0],"skill_instructions");
    assert!(named_skill.contains(commonmark) && named_skill.contains("@uses"),"a hook gets the skill its contract names: {named_skill}");
    assert!(!judge[0].to_string().contains(testing),"and no other");
    let count=of("count");
    assert_eq!(count.len(),1);
    assert!(named(&count[0],"skill_instructions").is_empty() && named(&count[0],"saved_asset").is_empty(),"a hook naming no skill has none");
    assert!(!count[0].to_string().contains(commonmark) && named(&count[0],"project_skills").is_empty(),"nothing passes down from its caller");
    let check=of("check");
    assert_eq!(check.len(),2,"the module is written, refused by the compiler, and written again");
    assert!(!check[0].to_string().contains(commonmark) && !check[0].to_string().contains(testing),"a called module does not inherit its caller's skills");
    assert!(check[0]["tools"].as_array().unwrap().iter().any(|t|t["function"]["name"]=="read_skill"),"its author can read the skills it will require");
    assert!(named(&check[1],"skill_instructions").contains(testing),"the code it wrote requires a skill, so its author gets it");
    let links=agent_service::open_db(&root).unwrap().prepare("SELECT data FROM agent_task_events WHERE task_id='example' AND kind='program/require'").unwrap()
        .query_map([],|r|r.get::<_,String>(0)).unwrap().map(|row|serde_json::from_str::<Value>(&row.unwrap()).unwrap()).collect::<Vec<_>>();
    assert!(links.iter().any(|link|link["library"]=="commonmark" && link["failed"]==false && link["result"]["methods"]==json!({})),"a skill without methods is required for its instructions: {links:?}");
    let status:String=agent_service::open_db(&root).unwrap().query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0)).unwrap();
    assert_eq!(status,"completed");
    std::fs::remove_dir_all(&root).ok();
}

/// What the user types in each turn of docs/multi-turn-experiment-plan.md.
const MINISQL_TURNS: [&str; 5] = [
    "在当前目录用 Python 写一个小型 SQL 数据库 minisql，只用标准库，不能用 sqlite3（写测试时可以拿 Python 自带的 sqlite3 对照）。接口：minisql.connect(\":memory:\") 返回一个内存库的连接（这一步只要内存库）；conn.execute(sql) 一次执行一条语句，SELECT 返回结果行的列表，每行是 tuple，和 sqlite3 的 fetchall() 一样；conn.close() 关闭连接；SQL 有错时抛 minisql.Error。这一步先支持：CREATE TABLE（INTEGER、REAL、TEXT 列）；INSERT（可以指定列，可以一次插多行）；SELECT（列、*、AS 别名、WHERE，也可以不带 FROM）；表达式有算术 + - * / %、字符串拼接 ||、比较 = != <> < <= > >=、AND OR NOT、IS NULL、IS NOT NULL、IN (值列表)、BETWEEN、LIKE。结果、NULL 的处理和值的类型都要和 SQLite 一致。写单元测试并确保通过。",
    "接着做：SELECT 支持 ORDER BY（多列、ASC/DESC）、LIMIT 和 OFFSET、DISTINCT，聚合函数 COUNT(*)、COUNT(列)、SUM、AVG、MIN、MAX，以及 GROUP BY 和 HAVING。结果和 SQLite 一致，之前的功能不能坏。",
    "再加上 UPDATE、DELETE，多表查询 INNER JOIN 和 LEFT JOIN（ON 条件，表可以起别名，列可以写成 表.列），以及 WHERE 里的 IN (子查询)。结果和 SQLite 一致，之前的功能不能坏。",
    "现在要能存盘：connect 传文件路径时打开这个文件，没有就新建；close 之后重新 connect 同一个文件，表和数据都还在。再加上事务：语句默认立即生效；BEGIN 开始事务，COMMIT 提交，ROLLBACK 撤销到 BEGIN 之前；事务没提交就 close，改动不保存；事务的行为和 SQLite 一致。之前的功能不能坏。",
    "有用户反馈这几条和 SQLite 的结果不一致，请修好：\n1. SELECT -7 / 2, -7 % 3 应该得到 (-3, -1)；\n2. score 列有 NULL 时，ORDER BY score 应该把 NULL 排在最前面，ORDER BY score DESC 排在最后；\n3. SELECT 1 / 0 应该得到 NULL，不报错。\n另外，10 万行的表按某一列等值查询太慢：加上 CREATE INDEX 索引名 ON 表 (列)，等值查询走索引；建了索引以后，增删改、事务回滚和重新打开文件之后，查询结果都要和 SQLite 一致。之前的功能不能坏。",
];

/// What the user adds while turn 3's program runs.
const MINISQL_INTERJECTION: &str = "另外 SELECT 里也要支持 CASE WHEN … THEN … ELSE … END（ELSE 可以省略），和 SQLite 一样。";

#[tokio::test]
#[ignore = "real configured model, isolated workspace, consumes provider tokens"]
async fn real_model_builds_a_sql_database_over_five_turns() -> Result<()> {
    let (root,state,model)=real_with_effort("minisql-real","medium")?;
    install_skills(&root,&["spec-driven-testing"])?;
    let snapshots=root.with_file_name(format!("{}-turns",root.file_name().and_then(|n|n.to_str()).unwrap_or("minisql-real")));
    eprintln!("REAL_MINISQL_START model={model} root={} snapshots={}",root.display(),snapshots.display());
    let started=std::time::Instant::now();
    let (mut unfinished,mut score)=(Vec::new(),Value::Null);
    for (index,prompt) in MINISQL_TURNS.iter().enumerate() {
        let turn=index+1;
        // A follow-up message reopens the conversation, as the service does.
        agent_service::open_db(&root)?.execute("UPDATE agent_tasks SET status='running' WHERE id='example'",[])?;
        let before=last_event_seq(&root)?;
        let speaker=(turn==3).then(||tokio::spawn(interject_once_working(root.clone(),before)));
        let turn_started=std::time::Instant::now();
        let cancel=CancellationToken::new();
        let result=tokio::time::timeout(Duration::from_secs(3600),run_task(state.clone(),"example".into(),model.clone(),(*prompt).into(),300,cancel.clone(),turn,false)).await;
        if result.is_err(){cancel.cancel();}
        let sent=match speaker { Some(speaker)=>{ speaker.abort(); match speaker.await { Ok(Ok(seq))=>Some(seq), Ok(Err(error))=>{ eprintln!("REAL_MINISQL_INTERJECTION failed={error}"); None }, Err(_)=>None } }, None=>None };
        let after=last_event_seq(&root)?;
        let conn=agent_service::open_db(&root)?;
        let count=|sql:String|conn.query_row(&sql,[],|row|row.get::<_,i64>(0)).unwrap_or(-1);
        let range=format!("task_id='example' AND seq>{before} AND seq<={after}");
        let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|row|row.get(0))?;
        let texts=|sql:String|conn.prepare(&sql).and_then(|mut rows|rows.query_map([],|row|row.get::<_,Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()).unwrap_or_default().into_iter().flatten().collect::<Vec<_>>();
        let outcomes=texts(format!("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE {range} AND kind='assistant/message' AND json_extract(data,'$.actor')='program' ORDER BY seq"));
        let answer=texts(format!("SELECT json_extract(data,'$.message.content') FROM agent_task_events WHERE {range} AND kind='assistant/message' ORDER BY seq DESC LIMIT 1"));
        let skills=texts(format!("SELECT json_extract(data,'$.arguments') FROM agent_task_events WHERE {range} AND kind='tool/call' AND json_extract(data,'$.name')='read_skill' ORDER BY seq"));
        let files=walk_files(&root)?;
        let lines=|tests:bool|files.iter().filter(|file|file.ends_with(".py") && is_test_file(file)==tests)
            .map(|file|std::fs::read_to_string(root.join(file)).map(|text|text.lines().count()).unwrap_or(0)).sum::<usize>();
        eprintln!("REAL_MINISQL_TURN turn={turn} seconds={:.0} status={status} model_requests={} programs={} runtime_errors={} function_calls={} failed_calls={} overflows={} history_reads={} read_skill={skills:?} code_lines={} test_lines={} files={files:?}",
            turn_started.elapsed().as_secs_f64(),
            count(format!("SELECT count(*) FROM agent_request_contexts WHERE task_id='example' AND turn={turn}")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='program/start' AND json_extract(data,'$.function') IS NULL")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='program/runtime_error'")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='task/node_planned' AND json_extract(data,'$.inputs.program_task') IS NOT NULL")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='task/node_returned' AND json_extract(data,'$.status')='failed'")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='task/node_returned' AND json_extract(data,'$.status')='failed' AND data LIKE '%CONTEXT_OVERFLOW%'")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='tool/call' AND json_extract(data,'$.name')='read_session_history'")),
            lines(false),lines(true));
        // Modules: written this turn, fixed, and calls that ran saved code,
        // some of it written in an earlier turn.
        eprintln!("REAL_MINISQL_MODULES turn={turn} written={} fixed_versions={} fix_attempts={} loaded={} loaded_from_earlier_turns={}",
            count(format!("SELECT count(*) FROM agent_modules WHERE task_id='example' AND turn={turn} AND version=1")),
            count(format!("SELECT count(*) FROM agent_modules WHERE task_id='example' AND turn={turn} AND fixed=1")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='module/fix_started'")),
            count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND kind='module/loaded'")),
            count(format!("SELECT count(*) FROM agent_task_events e WHERE e.{range} AND e.kind='module/loaded' AND EXISTS(SELECT 1 FROM agent_modules m WHERE m.task_id='example' AND m.contract=json_extract(e.data,'$.contract') AND m.version=1 AND m.turn<{turn})")));
        if turn==3 {
            let paused=outcomes.iter().any(|text|text.contains("被你的新消息暂停"));
            let resumed=sent.is_some_and(|seq|count(format!("SELECT count(*) FROM agent_task_events WHERE {range} AND seq>{seq} AND kind='program/start' AND data LIKE '%.resume(%'"))>0);
            eprintln!("REAL_MINISQL_INTERJECTION sent={} paused={paused} resumed={resumed}",sent.is_some());
        }
        score=minisql_score(&root,turn).await?;
        let groups=score["groups"].as_object().map(|groups|groups.iter().map(|(group,result)|format!("{group}:{}/{}",result["passed"],result["total"])).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        eprintln!("REAL_MINISQL_SCORE turn={turn} passed={}/{} groups=[{groups}] uses_sqlite3={} import_error={} failures={}",
            score["passed"],score["total"],score["uses_sqlite3"],score["import_error"],json!(score["failures"].as_array().map(|all|all.iter().take(8).collect::<Vec<_>>())));
        eprintln!("REAL_MINISQL_OUTCOME turn={turn} outcome={:?} answer={:?}",outcomes.last().map(|text|clip(text,600)),answer.first().map(|text|clip(text,600)));
        let saved=snapshots.join(format!("turn-{turn}"));
        for file in &files {
            let target=saved.join(file);
            std::fs::create_dir_all(target.parent().unwrap_or(&saved))?;
            std::fs::copy(root.join(file),target)?;
        }
        std::fs::create_dir_all(&saved)?;
        std::fs::write(saved.join("score.json"),serde_json::to_vec_pretty(&score)?)?;
        if !matches!(result,Ok(Ok(()))) || status!="completed" { unfinished.push(turn); }
    }
    eprintln!("REAL_MINISQL_DONE seconds={:.0} unfinished={unfinished:?} root={}",started.elapsed().as_secs_f64(),root.display());
    ensure!(unfinished.is_empty(),"turns that did not complete: {unfinished:?}");
    ensure!(score["uses_sqlite3"]==false,"minisql loads sqlite3");
    let (passed,total)=(score["passed"].as_u64().unwrap_or(0),score["total"].as_u64().unwrap_or(1));
    ensure!(passed*10>=total*8,"passed {passed} of {total} hidden cases");
    Ok(())
}

fn last_event_seq(root: &Path) -> Result<i64> {
    Ok(agent_service::open_db(root)?.query_row("SELECT COALESCE(MAX(seq),0) FROM agent_task_events WHERE task_id='example'",[],|row|row.get(0))?)
}

/// The user speaks once the turn's program calls a function, or ninety
/// seconds after a program starts without calling one.
async fn interject_once_working(root: std::path::PathBuf, before: i64) -> Result<i64> {
    let mut running_since=None;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let conn=agent_service::open_db(&root)?;
        let seen=|kind:&str,extra:&str|conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM agent_task_events WHERE task_id='example' AND seq>{before} AND kind='{kind}'{extra})"),[],|row|row.get::<_,bool>(0));
        let called=seen("task/node_planned"," AND json_extract(data,'$.inputs.program_task') IS NOT NULL")?;
        if running_since.is_none() && seen("program/start","")? { running_since=Some(std::time::Instant::now()); }
        if called || running_since.is_some_and(|since:std::time::Instant|since.elapsed()>Duration::from_secs(90)) {
            return crate::interjections::submit(&root,"example",MINISQL_INTERJECTION);
        }
    }
}

fn is_test_file(path: &str) -> bool {
    let name=Path::new(path).file_name().and_then(|n|n.to_str()).unwrap_or("");
    name.starts_with("test") || name.ends_with("_test.py") || Path::new(path).components().any(|part|part.as_os_str()=="tests")
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Scores the workspace's minisql with the hidden cases of every turn so far.
async fn minisql_score(root: &Path, turn: usize) -> Result<Value> {
    let hidden=std::env::temp_dir().join(format!("minisql-acceptance-{}.py",agent_service::uuid_like()));
    std::fs::write(&hidden,include_str!("fixtures/minisql_acceptance.py"))?;
    let run=tokio::process::Command::new("python3").arg("-I").arg(&hidden).arg(root).arg(turn.to_string()).kill_on_drop(true).output();
    let output=tokio::time::timeout(Duration::from_secs(1200),run).await;
    std::fs::remove_file(&hidden).ok();
    let output=output.context("hidden acceptance timed out")??;
    serde_json::from_slice(&output.stdout).with_context(||format!("hidden acceptance did not run: {}",String::from_utf8_lossy(&output.stderr)))
}

/// The hidden cases agree with SQLite itself: a thin wrapper over sqlite3
/// passes every one, no case contradicts its own marks, and the check that
/// minisql does not load sqlite3 catches the wrapper.
#[tokio::test]
async fn minisql_acceptance_agrees_with_sqlite() {
    let root=std::env::temp_dir().join(format!("minisql-reference-{}",agent_service::uuid_like()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("minisql.py"),"import sqlite3\n\n\nclass Error(Exception):\n    pass\n\n\nclass Connection:\n    def __init__(self, path):\n        self._conn = sqlite3.connect(path, isolation_level=None)\n\n    def execute(self, sql):\n        try:\n            return self._conn.execute(sql).fetchall()\n        except sqlite3.Error as error:\n            raise Error(str(error)) from error\n\n    def close(self):\n        self._conn.close()\n\n\ndef connect(path):\n    return Connection(path)\n").unwrap();
    let score=minisql_score(&root,5).await.unwrap();
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(score["broken"],json!([]),"cases whose marks SQLite contradicts");
    assert_eq!(score["failures"],json!([]));
    assert_eq!(score["passed"],score["total"]);
    assert!(score["total"].as_u64().unwrap()>100,"{score}");
    let mut groups=score["groups"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    groups.sort();
    assert_eq!(groups,["1","2","3","4","5","case"],"turn 5 scores every turn's cases");
    assert_eq!(score["uses_sqlite3"],true);
}

#[tokio::test]
#[ignore = "real model multi-turn mini webpack architecture experiment"]
async fn real_model_builds_mini_webpack_over_four_turns() -> Result<()> {
    let root=std::path::PathBuf::from(std::env::var("MINI_WEBPACK_ROOT").context("set MINI_WEBPACK_ROOT to an empty experiment directory")?);
    ensure!(root.is_dir(),"experiment directory does not exist");
    ensure!(std::fs::read_dir(&root)?.next().is_none(),"experiment directory must be empty");
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    state.reasoning_effort=Some(real_effort("medium"));
    let model=std::env::var("REAL_MODEL").ok().filter(|m|!m.is_empty()).unwrap_or_else(||state.model_map.get(&state.default_model).cloned().unwrap_or_else(||state.default_model.clone()));
    ensure!(!state.provider_url.is_empty() && !state.api_key.is_empty(),"configured model unavailable");
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','',?1,'running',0,0)",[&model])?;
    let snapshots=std::path::PathBuf::from(std::env::var("MINI_WEBPACK_EVIDENCE").context("set MINI_WEBPACK_EVIDENCE")?);
    std::fs::create_dir_all(&snapshots)?;
    let prompts=[
        "在当前目录用 Node.js 实现一个小型 JavaScript 打包工具。入口命令固定为 node mini-webpack.js <入口文件绝对路径> <输出文件绝对路径>。支持相对路径的 .js 文件，递归解析 ES module 的命名 import 和 export，把依赖打成一个可用 node 直接运行的 JS 文件；共享依赖只执行一次。自动创建输出目录，明确报告错误。可以使用 npm 包，但不能调用现成打包器如 webpack、esbuild、rollup、parcel。编写测试并运行，提供使用说明。",
        "继续完善：支持默认导入导出、命名导入别名、export { ... } from 重导出；支持将 JSON 文件作为默认导入。保持已有行为和测试，补充测试。",
        "继续完善：支持 import 引入 CSS；打包结果保留 CSS 内容，可以输出到同目录 CSS 文件或在浏览器运行时注入。原有 JavaScript 和 JSON 功能不回退，补充测试。",
        "请针对以下独立验收反馈复现并修复，增加回归测试；如果没有失败项，请检查并修复缺失模块时的错误提示：命令必须非零退出，错误包含缺失路径。最后重新运行所有测试并明确说明遗留问题。验收结果：",
    ];
    let mut prior=String::new();
    eprintln!("MINI_WEBPACK_START root={} evidence={} model={model}",root.display(),snapshots.display());
    for (i,prompt) in prompts.iter().enumerate() {
        let turn=i+1; agent_service::open_db(&root)?.execute("UPDATE agent_tasks SET status='running' WHERE id='example'",[])?;
        let before=last_event_seq(&root)?;
        let speaker=if turn==3 { let root=root.clone(); Some(tokio::spawn(async move {
            loop { tokio::time::sleep(Duration::from_millis(500)).await; let conn=agent_service::open_db(&root)?;
                let called:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_task_events WHERE task_id='example' AND seq>?1 AND kind='task/node_planned' AND json_extract(data,'$.inputs.program_task') IS NOT NULL)",[before],|r|r.get(0))?;
                if called { return crate::interjections::submit(&root,"example","另外，相对模块路径没有扩展名时也要能找到对应的 .js 文件，并补测试。"); }
            }
        })) } else {None};
        let actual=if turn==4 {format!("{prompt}\n{prior}")} else {(*prompt).to_owned()};
        let started=std::time::Instant::now(); let cancel=CancellationToken::new();
        let result=tokio::time::timeout(Duration::from_secs(3600),run_task(state.clone(),"example".into(),model.clone(),actual,300,cancel.clone(),turn,false)).await;
        if result.is_err(){cancel.cancel();}
        let sent=if let Some(s)=speaker {s.abort(); matches!(s.await,Ok(Ok(_)))} else {false};
        let after=last_event_seq(&root)?; let conn=agent_service::open_db(&root)?;
        let count=|kind:&str| conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND seq>?1 AND seq<=?2 AND kind=?3",rusqlite::params![before,after,kind],|r|r.get::<_,i64>(0)).unwrap_or(-1);
        let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|r|r.get(0))?;
        let check=tokio::process::Command::new("python").arg("-I").arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/single_agent/fixtures/mini_webpack_acceptance.py")).arg(&root).arg(turn.to_string()).output().await?;
        prior=String::from_utf8_lossy(&check.stdout).to_string();
        let save=snapshots.join(format!("turn-{turn}")); std::fs::create_dir_all(&save)?;
        for file in walk_files(&root)? {let target=save.join(&file); std::fs::create_dir_all(target.parent().unwrap())?; std::fs::copy(root.join(file),target)?;}
        std::fs::write(save.join("score.json"),&prior)?;
        let summary=json!({"turn":turn,"seconds":started.elapsed().as_secs_f64(),"status":status,"run_ok":matches!(result,Ok(Ok(()))),"interjection_sent":sent,"programs":count("program/start"),"module_loads":count("module/loaded"),"module_fixes":count("module/fix_started"),"runtime_errors":count("program/runtime_error"),"acceptance_exit":check.status.code(),"acceptance_stderr":String::from_utf8_lossy(&check.stderr),"score":serde_json::from_str::<Value>(&prior).unwrap_or(Value::Null)});
        std::fs::write(save.join("summary.json"),serde_json::to_vec_pretty(&summary)?)?;
        eprintln!("MINI_WEBPACK_TURN {summary}");
        if status!="completed" || !matches!(result,Ok(Ok(()))) { eprintln!("MINI_WEBPACK_STOP turn={turn}"); break; }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "controlled real-model feature probe; consumes provider tokens"]
async fn real_model_small_feature_probe() -> Result<()> {
    let root=std::path::PathBuf::from(std::env::var("FEATURE_PROBE_ROOT")?);
    let model=std::env::var("REAL_MODEL")?;
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state); state.reasoning_effort=Some(std::env::var("FEATURE_PROBE_EFFORT").unwrap_or_else(|_|"medium".into()));
    ensure!(!state.provider_url.is_empty() && !state.api_key.is_empty(),"configured model unavailable");
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at) VALUES ('example','',?1,'running',0,0)",[&model])?;
    let mode=std::env::var("FEATURE_PROBE_MODE").unwrap_or_default();
    if mode=="skill" { install_skills(&root,&["spec-driven-testing"])?; }
    let mut prompt="请在现有 mini-webpack 上增加一个小功能：运行 node mini-webpack.js --version 时，只输出 package.json 中的 version 和一个换行，并以状态码 0 退出，不进行构建。原有打包命令的行为不变。请添加自动化测试，运行相关测试，并简要说明结果。".to_owned();
    if mode=="nudge" { prompt.push_str("\n如果技能列表为空，表示没有可用技能，请继续执行任务，不要重复查询相同的技能列表。"); }
    if mode=="program" { prompt.push_str("\n请立即输出一段符合宿主要求的完整 JavaScript 程序。你可以在主程序中声明 @module，让它用代码组织读取、实现和测试，模块内需要 AI 判断时再用 @virtual。不要只回复会去做。"); }
    let cancel=CancellationToken::new(); let monitor_cancel=cancel.clone(); let monitor_root=root.clone();
    let monitor=tokio::spawn(async move { loop { tokio::time::sleep(Duration::from_secs(2)).await;
        let Ok(conn)=agent_service::open_db(&monitor_root) else {continue};
        let Ok(mut stmt)=conn.prepare("SELECT json_extract(data,'$.name'),json_extract(data,'$.arguments') FROM agent_task_events WHERE task_id='example' AND kind='tool/call' ORDER BY seq DESC LIMIT 10") else {continue};
        let Ok(rows)=stmt.query_map([],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?))) else {continue};
        let recent=rows.filter_map(|r|r.ok()).collect::<Vec<_>>();
        if recent.len()==10 && recent.iter().all(|v|v.0.as_deref()==Some("list_skills") && v.1.as_deref()==Some("{}")) { eprintln!("FEATURE_PROBE_REPEAT ten consecutive list_skills calls; cancelling"); monitor_cancel.cancel(); break; }
    }});
    eprintln!("FEATURE_PROBE_START root={} model={model}",root.display());
    let start=std::time::Instant::now(); let result=tokio::time::timeout(Duration::from_secs(600),run_task(state,"example".into(),model.clone(),prompt.into(),160,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();} monitor.abort();
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|r|r.get(0))?;
    let calls:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='tool/call' AND json_extract(data,'$.name')='list_skills'",[],|r|r.get(0))?;
    let programs:i64=conn.query_row("SELECT count(*) FROM agent_task_events WHERE kind='program/start'",[],|r|r.get(0))?;
    eprintln!("FEATURE_PROBE_RESULT {}",json!({"model":model,"status":status,"seconds":start.elapsed().as_secs_f64(),"run_ok":matches!(result,Ok(Ok(()))),"cancelled":cancel.is_cancelled(),"list_skills_calls":calls,"programs":programs}));
    Ok(())
}


#[tokio::test]
#[ignore = "real-model webpack follow-up in existing session; consumes provider tokens"]
async fn real_model_continues_mini_webpack_high_turn_five() -> Result<()> {
    let root=std::path::PathBuf::from(std::env::var("MINI_WEBPACK_ROOT").context("MINI_WEBPACK_ROOT is required")?);
    ensure!(root.join("mini-webpack.js").is_file(),"existing project not found");
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config); real_wire(&mut state);
    state.reasoning_effort=Some(real_effort("high"));
    let model=std::env::var("REAL_MODEL").unwrap_or_else(|_|"gpt-6-luna".into());
    ensure!(!state.provider_url.is_empty() && !state.api_key.is_empty(),"configured model unavailable");
    let conn=agent_service::open_db(&root)?;
    let (status,previous_model):(String,String)=conn.query_row("SELECT status,model FROM agent_tasks WHERE id='example'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(status=="completed","previous session status is {status}");
    ensure!(previous_model==model,"session model mismatch: {previous_model}");
    conn.execute("UPDATE agent_tasks SET status='running' WHERE id='example'",[])?; drop(conn);
    let before=last_event_seq(&root)?;
    let turn=std::env::var("MINI_WEBPACK_TURN").ok().and_then(|v|v.parse::<usize>().ok()).unwrap_or(5);
    let default_prompt="继续开发现有 mini-webpack，并保留此前 JS、JSON、CSS、无扩展名解析及 CLI 的行为。修复和扩展：1. 支持相对 JavaScript 模块的副作用导入 import './init.js'，按照依赖顺序执行，多个路径共享的模块只初始化一次；2. 支持 import * as name from './module.js'，命名空间同时暴露命名导出与默认导出，可与其他形式导入共享同一模块实例；3. 支持相对 JS 模块循环依赖中的函数导出和延迟函数调用：各模块只执行一次，循环引用建立后从入口调用函数的结果和 Node.js ES modules 一致。这里先不要求完整的 ES module 时间死区或所有 live binding 语义，但需写明当前限制。请先理解现有代码，补充自动化测试，运行完整回归测试，并更新 README 中已经过时的功能说明。";
    let prompt=match std::env::var("MINI_WEBPACK_PROMPT_FILE") { Ok(path)=>std::fs::read_to_string(path)?, Err(_)=>default_prompt.to_owned() };
    eprintln!("MINI_WEBPACK_HIGH_START root={} turn={turn} model={model} effort={:?}",root.display(),state.reasoning_effort);
    let started=std::time::Instant::now(); let cancel=CancellationToken::new();
    let result=tokio::time::timeout(Duration::from_secs(3600),run_task(state,"example".into(),model,prompt,300,cancel.clone(),turn,false)).await;
    if result.is_err(){cancel.cancel();}
    let conn=agent_service::open_db(&root)?;let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|r|r.get(0))?;
    let count=|kind:&str|conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND seq>?1 AND kind=?2",rusqlite::params![before,kind],|r|r.get::<_,i64>(0)).unwrap_or(-1);
    eprintln!("MINI_WEBPACK_HIGH_RESULT {}",json!({"status":status,"seconds":started.elapsed().as_secs_f64(),"run_ok":matches!(result,Ok(Ok(()))),"programs":count("program/start"),"runtime_errors":count("program/runtime_error"),"module_loads":count("module/loaded"),"module_fixes":count("module/fix_started")}));
    if !matches!(result,Ok(Ok(()))) || status!="completed" { anyhow::bail!("follow-up did not complete"); }
    Ok(())
}

/// Opt-in end-to-end analysis of a caller-prepared snapshot of this repository.
/// Uses the real provider, normal prompts, tool dispatcher and Agent Program VM.
/// The optional naive comparison changes only test-built main requests; it
/// uses the same production tool loop, provider, catalog and read permissions.
pub(super) fn repository_review_naive(s: &Session<'_>) -> bool {
    s.task == "example" && std::env::var("SELF_REVIEW_MODE").as_deref() == Ok("naive")
}

const NAIVE_REVIEW_PROMPT: &str = "Complete the user's authorized task using actual evidence and applicable skills. Use the advertised tools directly in a normal observe/reason/tool-call loop, then answer the user. Read library instructions and exact tool contracts when needed. For project analysis, discover the actual stack and entry points, follow source references, and read relevant implementations with code_index.read where supported. Base conclusions on complete relevant code, including conditions and earlier returns. Keep confirmed facts, inferences and missing evidence distinct. Work within the user's scope and read-only restrictions. Give a clear Chinese answer supported by file paths and line numbers. Respect pauses and required user decisions; report errors and incomplete work honestly. This test uses direct tool calls: do not submit or execute an orchestration program or delegate AI subfunctions.";

pub(super) fn repository_review_main_prompt(s: &Session<'_>) -> &'static str {
    if repository_review_naive(s) { NAIVE_REVIEW_PROMPT } else { MAIN_PROMPT }
}

#[tokio::test]
#[ignore = "real configured model, repository snapshot, consumes provider tokens"]
async fn real_model_analyzes_repository_snapshot() -> Result<()> {
    let root=std::path::PathBuf::from(std::env::var("SELF_REVIEW_ROOT").context("SELF_REVIEW_ROOT is required")?);
    ensure!(root.join("src/agent_program/runtime.rs").is_file(),"repository snapshot is missing");
    let evidence=root.parent().context("snapshot must have an evidence directory")?;
    let provider=std::env::var("REAL_PROVIDER").unwrap_or_else(|_|"deepseek".into());
    let model=std::env::var("REAL_MODEL").unwrap_or_else(|_|"deepseek-flash".into());
    let mut state=agent_service::tests::test_state(&root,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config);
    ensure!(state.use_provider(&provider),"provider {provider} is not configured");
    ensure!(state.model_map.contains_key(&model),"model {model} is not configured for {provider}");
    state.reasoning_effort=Some(real_effort("high"));
    state.permission_mode=agent_service::PermissionMode::ReadOnly;
    real_wire(&mut state);
    // Opt into the production native packages for library-integration trials.
    // Older isolated replays deliberately have no package catalog.
    let package_runtime = if std::env::var("SELF_REVIEW_WITH_PACKAGES").as_deref() == Ok("1") {
        use crate::plugin_runtime::{Plugin, PluginContext, PluginRuntime, ROOT};
        use std::sync::Arc;
        struct Services(Arc<crate::builtin_plugins::ToolCatalog>, Arc<crate::tools::Workspace>);
        impl Plugin for Services {
            fn id(&self) -> &str { "repository-test-services" }
            fn start(&self, ctx: &PluginContext<'_>) -> Result<()> {
                ctx.provide("tools", self.0.clone())?;
                ctx.provide("skill-packages", self.0.packages.clone())?;
                ctx.provide("workspace", self.1.clone())
            }
        }
        let runtime = PluginRuntime::new();
        let catalog = Arc::new(crate::builtin_plugins::ToolCatalog::default());
        let parent = runtime.mount(ROOT, Services(catalog.clone(), state.workspace.clone()))?;
        runtime.mount(parent, crate::builtin_plugins::FileToolsPlugin)?;
        runtime.mount(parent, crate::builtin_plugins::MemoryPlugin)?;
        for package in crate::builtin_plugins::packages()? {
            runtime.mount(parent, crate::skill_packages::PackagePlugin(package))?;
        }
        state.tool_catalog = Some(catalog);
        Some(runtime)
    } else { None };
    let effort=state.reasoning_effort.clone();
    let prompt=match std::env::var("SELF_REVIEW_PROMPT_FILE") {
        Ok(path)=>std::fs::read_to_string(path)?,
        Err(_)=>"分析一下这个项目的代码：梳理核心架构、从用户请求到模型和工具执行再到结果返回的主要链路，以及值得关注的问题。请依据实际读到的代码给出文件路径和行号，区分已经证实的问题、推测与没有验证的部分。只读分析，不要修改任何项目文件。".to_owned(),
    };
    let prompt=match std::env::var("SELF_REVIEW_PROMPT_SUFFIX_FILE") {
        Ok(path)=>format!("{prompt}\n\n{}",std::fs::read_to_string(path)?),
        Err(_)=>prompt,
    };
    std::fs::write(evidence.join("prompt.txt"),&prompt)?;
    agent_service::open_db(&root)?.execute("INSERT INTO agent_tasks(id,prompt,model,status,created_at,updated_at,provider_name,reasoning_effort) VALUES ('example',?1,?2,'running',0,0,?3,?4)",rusqlite::params![prompt,model,provider,effort])?;
    eprintln!("SELF_REVIEW_START provider={provider} model={model} effort={effort:?} root={}",root.display());
    let cancel=CancellationToken::new();let started=std::time::Instant::now();
    let result=tokio::time::timeout(Duration::from_secs(900),run_task(state,"example".into(),model.clone(),prompt,100,cancel.clone(),1,false)).await;
    if result.is_err(){cancel.cancel();}
    let conn=agent_service::open_db(&root)?;
    let status:String=conn.query_row("SELECT status FROM agent_tasks WHERE id='example'",[],|r|r.get(0))?;
    let count=|kind:&str|conn.query_row("SELECT count(*) FROM agent_task_events WHERE task_id='example' AND kind=?1",[kind],|r|r.get::<_,i64>(0)).unwrap_or(-1);
    let requests:i64=conn.query_row("SELECT count(*) FROM agent_request_contexts WHERE task_id='example'",[],|r|r.get(0))?;
    let error=match &result {Ok(Ok(()))=>None,Ok(Err(e))=>Some(format!("{e:#}")),Err(e)=>Some(e.to_string())};
    let answer=last_answer(&root).unwrap_or_default();
    std::fs::write(evidence.join("analysis.md"),&answer)?;
    let summary=json!({"provider":provider,"model":model,"reasoning_effort":effort,"mode":std::env::var("SELF_REVIEW_MODE").unwrap_or_else(|_|"orchestration".into()),"native_packages":package_runtime.is_some(),"status":status,"seconds":started.elapsed().as_secs_f64(),"run_ok":matches!(result,Ok(Ok(()))),"error":error,"model_requests":requests,"tool_calls":count("tool/call"),"programs":count("program/start"),"runtime_errors":count("program/runtime_error"),"module_loads":count("module/loaded"),"module_fixes":count("module/fix_started"),"answer_bytes":answer.len(),"workspace":root});
    std::fs::write(evidence.join("summary.json"),serde_json::to_vec_pretty(&summary)?)?;
    eprintln!("SELF_REVIEW_RESULT {summary}");
    if let Some(runtime) = package_runtime { runtime.shutdown()?; }
    ensure!(matches!(result,Ok(Ok(()))) && status=="completed","repository analysis did not complete");
    ensure!(!answer.trim().is_empty(),"repository analysis returned no answer");
    Ok(())
}

/// A single request with only the user's minimal Node.js planning instruction.
/// No production agent prompts, tool loop, implementation author or VM execution.
#[tokio::test]
#[ignore = "real configured model, minimal Node.js plan only, consumes provider tokens"]
async fn real_model_node_analysis_plan_minimal_prompt() -> Result<()> {
    let evidence=std::path::PathBuf::from(std::env::var("NODE_PLAN_OUTPUT").context("NODE_PLAN_OUTPUT is required")?);
    std::fs::create_dir_all(&evidence)?;
    let mut state=agent_service::tests::test_state(&evidence,"127.0.0.1:1".parse()?);
    let config=crate::ai_proxy::load_config(&crate::ai_proxy::dsh_config_path())?;
    crate::ai_proxy::apply_agent_config(&mut state,&config);
    ensure!(state.use_provider("deepseek"),"deepseek is not configured");
    let model="deepseek-flash";
    ensure!(state.model_map.contains_key(model),"deepseek-flash is not configured");
    real_wire(&mut state);
    let prompt=match std::env::var("NODE_PLAN_PROMPT_FILE") {
        Ok(path)=>std::fs::read_to_string(path)?,
        Err(_)=>"请用 Node.js 代码编排如何分析 codex-workspace-mcp 项目的代码，项目路径是 /home/goodtime/Documents/project/codex-workspace-mcp。用代码描述流程如何做，需要的方法用 JSDoc @virtual 标记并说明其功能、参数和返回值，不必实现。只返回代码。".to_owned(),
    };
    let agent_core=std::env::var("NODE_PLAN_AGENT_CORE").as_deref()==Ok("1");
    let messages=if agent_core {
        json!([{"role":"system","content":MAIN_PROMPT},
            {"role":"system","name":"program_environment","content":crate::agent_program::ENVIRONMENT},
            {"role":"user","content":prompt}])
    } else {json!([{"role":"user","content":prompt}])};
    let roles=messages.as_array().unwrap().iter().map(|m|m["role"].clone()).collect::<Vec<_>>();
    let effort=real_effort("low");
    let body=json!({"model":model,"messages":messages,"stream":false,"reasoning_effort":effort});
    std::fs::write(evidence.join("request.json"),serde_json::to_vec_pretty(&body)?)?;
    eprintln!("NODE_PLAN_START model={model} evidence={}",evidence.display());
    let started=std::time::Instant::now();
    let response=state.model_client.post(format!("{}/chat/completions",state.provider_url.trim_end_matches('/')))
        .bearer_auth(&state.api_key).json(&body).timeout(Duration::from_secs(300)).send().await?.error_for_status()?;
    let result:Value=response.json().await?;
    let raw=result["choices"][0]["message"]["content"].as_str().context("model returned no source")?;
    std::fs::write(evidence.join("response.txt"),raw)?;
    let source=raw.trim();
    let source=if source.starts_with("```") {
        source.split_once('\n').map(|(_,s)|s).unwrap_or(source).trim_end_matches('`').trim()
    } else {source};
    let source_file=if source.lines().any(|line| line.starts_with("import ") || line.starts_with("export ")) {
        "analysis-plan.mjs"
    } else { "analysis-plan.cjs" };
    std::fs::write(evidence.join(source_file),source)?;
    let report=json!({"provider":"deepseek","model":model,"seconds":started.elapsed().as_secs_f64(),
        "requests":1,"message_roles":roles,"prompt_mode":if agent_core {"agent-core"} else {"user-only"},
        "usage":result["usage"],"finish_reason":result["choices"][0]["finish_reason"],
        "lines":source.lines().count(),"chars":source.chars().count(),"source_file":source_file,"executed":false});
    std::fs::write(evidence.join("summary.json"),serde_json::to_vec_pretty(&report)?)?;
    eprintln!("NODE_PLAN_RESULT {report}");
    Ok(())
}
