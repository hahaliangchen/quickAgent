    #[test]
    fn empty_or_repeated_tool_metadata_is_not_generation_progress() {
        let mut stream = ChatStream::new(LIMITS, Instant::now());
        let first = stream
            .push(tool_event(0, Some("write_file"), "").as_bytes())
            .unwrap();
        assert!(first.tool_call);
        let repeated = stream
            .push(tool_event(0, Some("write_file"), "").as_bytes())
            .unwrap();
        assert!(repeated.is_empty());
        let arguments = stream
            .push(tool_event(0, None, "{\"content\":").as_bytes())
            .unwrap();
        assert!(arguments.tool_call);
        assert_eq!(stream.tool_progress(), json!([{"name":"write_file","argument_bytes":11}]));
        stream.push(tool_event(0, None, "\"你好\"").as_bytes()).unwrap();
        assert_eq!(stream.tool_progress(), json!([{"name":"write_file","argument_bytes":19}]));
    }

    fn responses_events(events: &[Value]) -> String {
        events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
    }

    #[test]
    fn a_responses_stream_is_the_same_message_and_keeps_its_reasoning_items() {
        let reasoning = json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Checking the files"}],"encrypted_content":"enc-1"});
        let stream = responses_events(&[
            json!({"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
            json!({"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":0}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"Checking the files"}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"a.txt\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}),
            json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":10,"output_tokens":5}}}),
        ]);
        let mut chat = ChatStream::new(Limits::WORKER, Instant::now());
        let delta = chat.push(stream.as_bytes()).unwrap();
        assert!(delta.tool_call && delta.reasoning == "Checking the files");
        assert!(chat.received_done(), "response.completed ends the stream");
        let completed = chat.finish().unwrap();
        let message = completed.message;
        assert_eq!(message["tool_calls"], json!([{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]));
        assert_eq!(message["reasoning_content"], "Checking the files");
        assert_eq!(message[REASONING_ITEMS], json!([reasoning]), "the encrypted reasoning goes back on the next request");
        assert_eq!(completed.stats["finish_reason"], "tool_calls");
        assert_eq!(completed.stats["usage"]["input_tokens"], 10);
        assert!(completed.terminated);
    }

    #[test]
    fn summary_parts_stream_as_the_paragraphs_the_final_message_keeps() {
        let part = |index: u64, text: &str| [
            json!({"type":"response.reasoning_summary_part.added","output_index":0,"summary_index":index}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":text}),
        ];
        let events = [part(0, "**Reading the file**"), part(1, "**Writing the fix**")].concat();
        let mut chat = ChatStream::new(Limits::WORKER, Instant::now());
        let streamed = events
            .iter()
            .map(|event| chat.push(responses_events(std::slice::from_ref(event)).as_bytes()).unwrap().reasoning)
            .collect::<String>();
        assert_eq!(streamed, "**Reading the file**\n\n**Writing the fix**");
        chat.push(responses_events(&[json!({"type":"response.completed","response":{"status":"completed"}})]).as_bytes()).unwrap();
        assert_eq!(chat.finish().unwrap().message["reasoning_content"], streamed);
    }

    #[test]
    fn a_responses_answer_failure_and_cut_off_read_as_their_chat_counterparts() {
        let answer = responses_events(&[
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"Done"}),
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"."}),
            json!({"type":"response.completed","response":{"status":"completed"}}),
        ]);
        let completed = feed(&[answer.as_bytes()]).unwrap();
        assert_eq!(completed.message["content"], "Done.");
        assert_eq!(completed.stats["finish_reason"], "stop");
        let cut = responses_events(&[json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}})]);
        assert_eq!(feed(&[cut.as_bytes()]).unwrap().stats["finish_reason"], "length");
        let failed = responses_events(&[json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"upstream broke"}}})]);
        let Err(error) = feed(&[failed.as_bytes()]) else { panic!("a failed response is an error") };
        assert_eq!(stream_error_code(&error), Some("UPSTREAM_ERROR"));
        assert!(error.to_string().contains("upstream broke"));
        // A provider that does not stream sends the whole response.
        let whole = json!({"object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Whole"}]}]});
        assert_eq!(feed(&[whole.to_string().as_bytes()]).unwrap().message["content"], "Whole");
    }

    #[test]
    fn a_chat_request_is_carried_over_to_the_responses_form() {
        let chat = json!({"model":"gpt-6-luna","stream":true,"parallel_tool_calls":false,"max_tokens":4096,"reasoning_effort":"medium","prompt_cache_key":"cache-k1",
            "messages":[
                {"role":"system","content":"You are the main AI."},
                {"role":"system","name":"program_tool_headers","content":"declare namespace io {}"},
                {"role":"user","content":"Add --version"},
                {"role":"assistant","content":null,REASONING_ITEMS:[{"type":"reasoning","id":"rs_1","encrypted_content":"enc-1"}],
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]},
                {"role":"tool","tool_call_id":"call_1","content":"{\"content\":\"alpha\"}"},
                {"role":"user","name":"program_diagnostic","content":"{\"code\":\"PROGRAM_NOT_RUN\"}"},
                {"role":"user","content":[{"type":"text","text":"see"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA","detail":"high"}}]},
            ],
            "tools":[{"type":"function","function":{"name":"read_file","description":"Read a file","parameters":{"type":"object"}}}]});
        let body = responses_request(&chat);
        assert_eq!(body["instructions"], "You are the main AI.");
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["reasoning"], json!({"effort":"medium","summary":"auto"}));
        assert_eq!(body["prompt_cache_key"], "cache-k1");
        assert!(body.get("max_output_tokens").is_none() && body.get("messages").is_none());
        assert_eq!(body["tools"], json!([{"type":"function","name":"read_file","description":"Read a file","parameters":{"type":"object"},"strict":false}]));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0], json!({"type":"message","role":"developer","content":[{"type":"input_text","text":"[program_tool_headers]\ndeclare namespace io {}"}]}));
        assert_eq!(input[1]["content"][0]["text"], "Add --version");
        assert_eq!(input[2], json!({"type":"reasoning","id":"rs_1","encrypted_content":"enc-1"}), "reasoning comes back before the call it led to");
        assert_eq!(input[3], json!({"type":"function_call","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}));
        assert_eq!(input[4], json!({"type":"function_call_output","call_id":"call_1","output":"{\"content\":\"alpha\"}"}));
        assert_eq!(input[5]["content"][0]["text"], "[program_diagnostic]\n{\"code\":\"PROGRAM_NOT_RUN\"}");
        assert_eq!(input[6]["content"][1], json!({"type":"input_image","image_url":"data:image/png;base64,AA","detail":"high"}));
    }

    #[test]
    fn prefix_system_projection_preserves_tool_pairs_and_instruction_order() {
        let messages=serde_json::json!([
            {"role":"system","content":"policy"},{"role":"user","content":"request"},
            {"role":"assistant","tool_calls":[{"id":"a"}]},{"role":"tool","tool_call_id":"a","content":"result"},
            {"role":"system","content":"capacity"}
        ]).as_array().unwrap().clone();
        let projected = super::prefix_system_messages(messages.clone());
        assert_eq!(projected[0]["content"], "policy\n\ncapacity");
        assert_eq!(&projected[1..], &messages[1..4]);
    }
    use super::*;

    const LIMITS: Limits = Limits {
        max_total_bytes: 1 << 20,
        max_line_bytes: 1 << 16,
        max_tool_argument_bytes: 4096,
        retain_reasoning: false,
    };

    fn feed(chunks: &[&[u8]]) -> anyhow::Result<Completed> {
        let mut stream = ChatStream::new(LIMITS, Instant::now());
        for chunk in chunks {
            stream.push(chunk)?;
        }
        stream.finish()
    }

    fn tool_event(index: usize, name: Option<&str>, arguments: &str) -> String {
        let mut function = json!({"arguments":arguments});
        if let Some(name) = name {
            function["name"] = json!(name);
        }
        let call = json!({"index":index,"id":if name.is_some() {json!("call_1")} else {Value::Null},"function":function});
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"delta":{"tool_calls":[call]}}]})
        )
    }

    #[test]
    fn tool_arguments_split_across_frames_and_utf8_boundaries_are_exact() {
        let arguments =
            json!({"goal":"上传演示文稿并确认页码","reason":"服务已返回 200"}).to_string();
        let (head, tail) = arguments.split_at(arguments.find('文').unwrap());
        let stream = format!(
            ": keepalive\n\n{}{}{}data: {}\n\ndata: [DONE]\n\n",
            tool_event(0, Some("schedule_task"), ""),
            tool_event(0, None, head),
            tool_event(0, None, tail),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"total_tokens":42}})
        );
        let bytes = stream.as_bytes();
        let split = stream.find('文').unwrap() + 1;
        // Split inside a multibyte character and inside an SSE line.
        let completed = feed(&[&bytes[..7], &bytes[7..split], &bytes[split..]]).unwrap();
        assert!(completed.terminated);
        let call = &completed.message["tool_calls"][0];
        assert_eq!(call["function"]["name"], "schedule_task");
        assert_eq!(
            serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
            serde_json::from_str::<Value>(&arguments).unwrap()
        );
        assert_eq!(completed.stats["response_format"], "sse");
        assert_eq!(completed.stats["received_done"], true);
        assert_eq!(completed.stats["finish_reason"], "tool_calls");
        assert_eq!(completed.stats["usage"]["total_tokens"], 42);
        assert_eq!(completed.stats["keepalives"], 1);
        assert!(completed.stats["first_tool_delta_ms"].is_u64());
    }

    #[test]
    fn eof_without_terminal_is_not_terminated() {
        let stream = tool_event(0, Some("schedule_task"), "{\"goal\":\"half");
        let completed = feed(&[stream.as_bytes()]).unwrap();
        assert!(!completed.terminated);
    }

    #[test]
    fn upstream_error_frame_and_malformed_events_fail() {
        let error = feed(&[b"data: {\"error\":{\"message\":\"overloaded\"}}\n\n"])
            .err()
            .unwrap();
        assert_eq!(stream_error_code(&error), Some("UPSTREAM_ERROR"));
        let error = feed(&[b"data: {not json\n\n"]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("MALFORMED_EVENT"));
        let error = feed(&[b"data: \xff\xfe\n\n"]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("INVALID_UTF8"));
    }

    #[test]
    fn plain_json_response_is_reported_as_json_without_stream_delta_times() {
        let body = json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c","type":"function",
            "function":{"name":"finish_request","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}).to_string();
        let completed = feed(&[&body.as_bytes()[..10], &body.as_bytes()[10..]]).unwrap();
        assert!(completed.terminated);
        assert_eq!(completed.stats["response_format"], "json");
        assert!(completed.stats["first_delta_ms"].is_null());
        assert_eq!(
            completed.message["tool_calls"][0]["function"]["name"],
            "finish_request"
        );
    }

    #[test]
    fn events_without_blank_separators_and_oversized_arguments() {
        let stream = format!(
            "data: {}\ndata: {}\ndata: [DONE]\n",
            json!({"choices":[{"delta":{"content":"a"}}]}),
            json!({"choices":[{"delta":{"content":"b"},"finish_reason":"stop"}]})
        );
        let completed = feed(&[stream.as_bytes()]).unwrap();
        assert_eq!(completed.message["content"], "ab");
        assert!(completed.terminated);
        let huge = tool_event(0, Some("schedule_task"), &"x".repeat(5000));
        let error = feed(&[huge.as_bytes()]).err().unwrap();
        assert_eq!(stream_error_code(&error), Some("ARGUMENTS_TOO_LARGE"));
    }

    #[test]
    fn run_protocol_parses_bare_js_and_return_heredoc_replies() {
        assert_eq!(
            parse_run_submission("const x = io.read({ path: \"a.rs\" });", Some("stop")),
            RunSubmission::Run {
                source: "const x = io.read({ path: \"a.rs\" });".into()
            }
        );
        assert_eq!(
            parse_run_submission("```js\nconst x = 1;\n```", Some("stop")),
            RunSubmission::Run {
                source: "const x = 1;".into()
            }
        );
        let reply = "return <<<\n检查完毕，示例代码：\n```js\nconst x = 1;\n```";
        assert_eq!(
            parse_run_submission(reply, Some("stop")),
            RunSubmission::Reply {
                markdown: "检查完毕，示例代码：\n```js\nconst x = 1;\n```".into()
            }
        );
        assert_eq!(
            parse_run_submission("const a = 1;\nreturn <<<\ndone\n>>>", Some("stop")),
            RunSubmission::CodeAndReply {
                source: "const a = 1;".into(),
                markdown: "done".into(),
            }
        );
        // Handles glued `;return <<<\n` without newline (e.g. multi-channel proxy concat).
        assert_eq!(
            parse_run_submission("const f = io.read({path:\"a.rs\"});return <<<\npremature", Some("stop")),
            RunSubmission::CodeAndReply {
                source: "const f = io.read({path:\"a.rs\"});".into(),
                markdown: "premature".into(),
            }
        );
        // Strips stray RL tag tails after valid JS.
        assert_eq!(
            parse_run_submission("const f = io.read({path:\"a.rs\"});\n</parameter>", Some("stop")),
            RunSubmission::Run {
                source: "const f = io.read({path:\"a.rs\"});".into(),
            }
        );
        assert!(matches!(
            parse_run_submission("const a = 1;", Some("length")),
            RunSubmission::Rejected {
                code: "OUTPUT_TRUNCATED",
                ..
            }
        ));
        assert!(matches!(
            parse_run_submission("return <<<\nhalf", Some("length")),
            RunSubmission::Rejected {
                code: "REPLY_TRUNCATED",
                ..
            }
        ));
    }

    #[test]
    fn run_stream_gate_separates_bare_js_from_return_heredoc_reply() {
        // Bare JS code never leaks into ReplyDelta.
        let mut code = RunStreamGate::default();
        assert_eq!(
            code.push("const f = io.read({});"),
            vec![RunStreamEvent::RunProgress("const f = io.read({});".into())]
        );
        assert!(code.is_code());

        // `return "x";` is also JS code, not a heredoc reply.
        let mut js_ret = RunStreamGate::default();
        assert!(js_ret.push("ret").is_empty());
        assert!(js_ret.push("urn ").is_empty());
        assert_eq!(
            js_ret.push("\"ok\";"),
            vec![RunStreamEvent::RunProgress("return \"ok\";".into())]
        );
        assert!(js_ret.is_code());

        // `return <<<\n` streams subsequent UTF-8 Markdown immediately as ReplyDelta.
        let mut reply = RunStreamGate::default();
        assert!(reply.push("ret").is_empty());
        assert!(reply.push("urn <<<").is_empty());
        assert_eq!(
            reply.push("\n你好世界"),
            vec![RunStreamEvent::ReplyDelta("你好世界".into())]
        );
        assert_eq!(
            reply.push("\n```js\nconst a = 1;\n```"),
            vec![RunStreamEvent::ReplyDelta("\n```js\nconst a = 1;\n```".into())]
        );
        assert!(!reply.is_code());
        assert!(reply.finish().is_empty());
    }

    #[test]
    fn complete_js_burst_detects_balanced_code_and_skips_incomplete_or_replies() {
        assert!(is_complete_js_burst(
            "const f = io.read({ path: \"src/app.ts\" });"
        ));
        assert!(is_complete_js_burst(
            "const a = 1;\nfunction foo(x) {\n  return `val=${x}`;\n}"
        ));
        assert!(is_complete_js_burst(
            "```js\nconst f = io.read({ path: \"a.ts\" });\n```"
        ));
        assert!(!is_complete_js_burst(""));
        assert!(!is_complete_js_burst("// just a comment"));
        assert!(!is_complete_js_burst("const x = { a: 1"));
        assert!(!is_complete_js_burst("const s = \"unclosed;"));
        assert!(!is_complete_js_burst("const t = `unclosed ${x};"));
        assert!(!is_complete_js_burst("const f = io.read({ path: \"a\" })"));
        assert!(!is_complete_js_burst("return <<<\nDone;"));
        assert!(!is_complete_js_burst("const x = 1;\nreturn <<<\nDone;"));
    }

    #[test]
    fn chat_stream_captures_first_js_burst_when_reasoning_resumes_mid_stream() {
        let mut stream = ChatStream::new(Limits::WORKER, Instant::now());
        // Reproduces gpt-6.1-terra interleaving reasoning -> JS -> reasoning -> duplicate JS.
        let burst1 = "const sidebarFile = io.read({path:\"frontend/src/components/Sidebar.tsx\"});\nconst statusNow = git.status({include_untracked:true});";
        let sse = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"choices":[{"delta":{"reasoning_content":"**Inspecting target files**"}}]}),
            json!({"choices":[{"delta":{"content": burst1}}]}),
            json!({"choices":[{"delta":{"reasoning_content":"**Waiting for host output**"}}]}),
            json!({"choices":[{"delta":{"content": burst1}}]}),
        );
        stream.push(sse.as_bytes()).unwrap();
        assert_eq!(stream.interrupted_code_burst(), Some(burst1));
        let completed = stream.finish_truncated_code_burst().unwrap();
        assert!(completed.terminated);
        assert_eq!(completed.message["content"].as_str(), Some(burst1));
        assert_eq!(completed.stats["truncated_on_reasoning_resume"], json!(true));
        assert_eq!(completed.stats["finish_reason"], json!("stop"));
    }

    #[test]
    fn chat_stream_does_not_truncate_return_heredoc_reply_when_reasoning_resumes() {
        let mut stream = ChatStream::new(Limits::WORKER, Instant::now());
        let sse = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":"return <<<\nFirst paragraph;"}}]}),
            json!({"choices":[{"delta":{"reasoning_content":"**Wrapping up**"}}]}),
            json!({"choices":[{"delta":{"content":"\nSecond paragraph."}}]}),
        );
        stream.push(sse.as_bytes()).unwrap();
        assert_eq!(stream.interrupted_code_burst(), None);
        let completed = stream.finish().unwrap();
        assert_eq!(
            completed.message["content"].as_str(),
            Some("return <<<\nFirst paragraph;\nSecond paragraph.")
        );
    }
