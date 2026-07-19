use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn model_judge_requires_an_explicit_local_route() {
    let task = task::load_local(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/college_english_exam_bank"),
    )
    .unwrap();
    let config = config::LocalConfig {
        path: None,
        runtime: crate::runtime_selection::RuntimeSelection::bench_default().unwrap(),
        judge_model: None,
    };
    let error = resolve_judge_model(&task, None, &config).err().unwrap();
    assert!(error.to_string().contains("requires bench.judge_model"));
}

#[test]
fn judge_identity_binds_the_model_route_without_credentials() {
    let model = JudgeModel {
        reference: "custom/grader".into(),
        route: config::ModelRoute {
            model: "grader".into(),
            api_key: "secret".into(),
            base_url: "https://example.test/v1".into(),
        },
    };
    let identity = judge_identity("judge-asset", Some(&model));
    assert_eq!(identity, "judge-asset;model=custom/grader");
    assert!(!identity.contains("secret"));
    assert!(!identity.contains("example.test"));
}

#[test]
#[ignore = "requires Docker and the linux/amd64 imported game images"]
fn model_candidate_game_and_task_owned_judge_run_end_to_end() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || serve_game_model(listener));
    let state = tempfile::tempdir().unwrap();
    let config_path = state.path().join(".a3s/config.acl");
    std::fs::create_dir(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        format!(
            "default_model = \"openai/fake\"\nproviders \"openai\" {{\n  api_key = \"test\"\n  base_url = \"http://{address}\"\n  models \"fake\" {{ name = \"Fake\" }}\n}}\n"
        ),
    )
    .unwrap();
    let config = config::discover(state.path()).unwrap();
    let task_source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/anchorhead_text_adventure");
    let task_lock_path = state.path().join("task.lock.json");
    lock::create_task_with_provider(&task_source, None, state.path(), &task_lock_path, "docker")
        .unwrap();
    let locked = lock::load_task(&task_lock_path, state.path()).unwrap();
    let mut task = task::load_local(&locked.task_artifact).unwrap();
    resolve_task_images(&mut task, &locked.lock.resolved_images).unwrap();
    let game = start_game(&task, state.path()).unwrap().unwrap();
    let candidate_root = state.path().join("candidate");
    std::fs::create_dir(&candidate_root).unwrap();
    std::fs::write(
        candidate_root.join("agent.md"),
        "---\nname: game-test\nmax_steps: 8\n---\n\nPlay the supplied game.\n",
    )
    .unwrap();
    let candidate = asset::LocalAssetPackage {
        root: candidate_root,
        entrypoint: "unused".into(),
        definition_path: Some("agent.md".into()),
        identity: "test-candidate".into(),
    };
    let candidate_workspace = workspace::create(&task).unwrap();
    let resolved_images = std::collections::BTreeMap::new();
    let runtime_execution = RuntimeExecution {
        provider: "docker",
        resolved_images: &resolved_images,
    };
    let execution = execute_candidate(
        &task,
        &candidate,
        Some("openai/fake"),
        &config,
        &candidate_workspace,
        Some(&game),
        &runtime_execution,
    )
    .unwrap()
    .unwrap();
    server.join().unwrap();
    assert_eq!(execution.tool_calls_count, 1);
    let submission = workspace::create_submission(&task, &candidate_workspace).unwrap();
    let judge = asset::load_local(&task.root.join(&task.judge_asset)).unwrap();
    let result = execute_judge(
        &task,
        &judge,
        &submission,
        Some(&game),
        None,
        "docker",
        &resolved_images,
    )
    .unwrap();
    assert_eq!(result.schema, "bench.judge.result.v1");
    assert_eq!(result.solution_verdict, "valid");
    assert!(result.diagnostics.get("moves").is_some());
}

fn serve_game_model(listener: TcpListener) {
    let messages = [
        serde_json::json!({
            "role":"assistant", "content":null,
            "tool_calls":[{"id":"call_1","type":"function","function":{
                "name":"bash",
                "arguments":serde_json::to_string(&serde_json::json!({
                    "cmd":"python -c \"import json,os,urllib.request; u=os.environ['GAME_SERVER_URL']+'/new'; r=urllib.request.Request(u,data=b'{}',headers={'Content-Type':'application/json'}); print(urllib.request.urlopen(r).read().decode())\""
                })).unwrap()
            }}]
        }),
        serde_json::json!({"role":"assistant","content":"Game started successfully."}),
    ];
    let mut response_index = 0;
    while response_index < messages.len() {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let body_start = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let request_body: serde_json::Value =
            serde_json::from_slice(&request[body_start..]).unwrap();
        if request_body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            continue;
        }
        let is_pre_analysis = request_body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|content| content.contains("You are a pre-analysis assistant"))
                })
            });
        let message = if is_pre_analysis {
            serde_json::json!({
                "role":"assistant",
                "content": serde_json::json!({
                    "intent": "GeneralPurpose",
                    "requires_planning": false,
                    "goal": {
                        "description": "Play the supplied game.",
                        "success_criteria": ["Start the game successfully"]
                    },
                    "execution_plan": {
                        "complexity": "Simple",
                        "steps": [{
                            "id": "step-1",
                            "description": "Start the supplied game",
                            "tool": "bash",
                            "dependencies": [],
                            "success_criteria": "The game server returns a session"
                        }],
                        "required_tools": ["bash"]
                    },
                    "optimized_input": "Play the supplied game."
                }).to_string()
            })
        } else {
            messages[response_index].clone()
        };
        let body = serde_json::to_vec(&serde_json::json!({
            "id":"chatcmpl-game-test", "object":"chat.completion", "created":0, "model":"fake",
            "choices":[{"index":0,"message":message,"finish_reason":if !is_pre_analysis && response_index == 0 {"tool_calls"} else {"stop"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        })).unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        if !is_pre_analysis {
            response_index += 1;
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + length {
            return request;
        }
    }
}
