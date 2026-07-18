use a3s_code_core::sandbox::{BashSandbox, SandboxOutput};
use a3s_code_core::{config::CodeConfig, Agent, PlanningMode, SessionOptions, WorkspaceServices};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExecution {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cache_read_tokens: Option<usize>,
    pub cache_write_tokens: Option<usize>,
    pub tool_calls_count: usize,
}

pub struct ModelCandidateRequest<'a> {
    pub config_path: &'a Path,
    pub model: &'a str,
    pub task_prompt: &'a str,
    pub candidate_instructions: &'a str,
    pub workspace: &'a Path,
    pub workspace_source_path: Option<&'a str>,
    pub work_image: &'a str,
    pub work_platform: Option<&'a str>,
    pub game_network: Option<(&'a str, &'a str)>,
    pub public_internet: bool,
    pub timeout_sec: u64,
    pub max_tool_rounds: usize,
}

pub fn execute(request: ModelCandidateRequest<'_>) -> Result<ModelExecution> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(execute_async(request))
}

async fn execute_async(request: ModelCandidateRequest<'_>) -> Result<ModelExecution> {
    let mut config = CodeConfig::from_file(request.config_path).with_context(|| {
        format!(
            "could not load model Candidate configuration from {}",
            request.config_path.display()
        )
    })?;
    config.default_model = Some(request.model.to_owned());
    let agent = Agent::from_config(config)
        .await
        .context("could not initialize selected model Candidate from config.acl")?;
    let workspace = request.workspace.canonicalize()?;
    let sandbox: Arc<dyn BashSandbox> = Arc::new(DockerBashSandbox {
        image: request.work_image.to_owned(),
        platform: request.work_platform.map(str::to_owned),
        workspace: workspace.clone(),
        game_network: request
            .game_network
            .map(|(network, url)| (network.to_owned(), url.to_owned())),
        public_internet: request.public_internet,
    });
    let options =
        candidate_session_options(request.model, &workspace, sandbox, request.max_tool_rounds);
    let session = agent
        .session_builder(workspace.display().to_string())
        .options(options)
        .build()
        .await
        .context("could not create model Candidate session")?;
    let prompt = candidate_prompt(&request);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(request.timeout_sec),
        session.send(&prompt, None),
    )
    .await;
    session.close().await;
    let result = result
        .context("model Candidate exceeded Task solution_timeout_sec")?
        .context("model Candidate execution failed")?;
    Ok(ModelExecution {
        prompt_tokens: result.usage.prompt_tokens,
        completion_tokens: result.usage.completion_tokens,
        total_tokens: result.usage.total_tokens,
        cache_read_tokens: result.usage.cache_read_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        tool_calls_count: result.tool_calls_count,
    })
}

fn candidate_prompt(request: &ModelCandidateRequest<'_>) -> String {
    format!(
        "{}\n\n# Benchmark task\n\n{}\n\n# Workspace contract\n\n{}\n\nComplete the task and verify the result.",
        request.candidate_instructions,
        request.task_prompt,
        workspace_contract(request.workspace_source_path)
    )
}

fn workspace_contract(source_path: Option<&str>) -> String {
    let Some(source_path) = source_path else {
        return "The supplied workspace is the editable submission root. Use workspace-relative paths with file tools and `/workspace` paths in Bash. Write deliverables only inside `/workspace`."
            .to_string();
    };
    let source_name = Path::new(source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("source directory");
    format!(
        "The editable workspace is the extracted contents of `{source_path}` and is mounted at `/workspace` for Bash. It is already the `{source_name}` directory: when task instructions name `{source_name}/path`, use `path` with file tools and `/workspace/path` in Bash; do not create another `{source_name}` directory. The work image may contain public, read-only task fixtures outside `{source_path}`. Bash may inspect those task-provided paths when required, but all deliverable writes must stay inside `/workspace`. Never pass the host workspace path to a shell command."
    )
}

fn candidate_session_options(
    model: &str,
    workspace: &Path,
    sandbox: Arc<dyn BashSandbox>,
    max_tool_rounds: usize,
) -> SessionOptions {
    SessionOptions::new()
        .with_model(model)
        .with_workspace_backend(WorkspaceServices::local(workspace))
        .with_sandbox_handle(sandbox)
        .with_confirmation_policy(a3s_code_core::hitl::ConfirmationPolicy::default())
        .with_file_memory(workspace.join(".a3s/memory"))
        .with_max_tool_rounds(max_tool_rounds)
        .with_planning_mode(PlanningMode::Auto)
        .with_continuation(true)
        .with_manual_delegation_enabled(true)
}

struct DockerBashSandbox {
    image: String,
    platform: Option<String>,
    workspace: PathBuf,
    game_network: Option<(String, String)>,
    public_internet: bool,
}

#[async_trait]
impl BashSandbox for DockerBashSandbox {
    async fn exec_command(&self, command: &str, guest_workspace: &str) -> Result<SandboxOutput> {
        anyhow::ensure!(
            guest_workspace == "/workspace",
            "unexpected sandbox workspace"
        );
        let mut docker = Command::new("docker");
        docker.args([
            "run",
            "--rm",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
        ]);
        docker.args(crate::runtime_profile::WORK_DOCKER_LIMITS);
        if let Some(platform) = self.platform.as_deref() {
            docker.args(["--platform", platform]);
        }
        if let Some((network, url)) = &self.game_network {
            docker.args([
                "--network",
                network,
                "--env",
                &format!("GAME_SERVER_URL={url}"),
            ]);
        } else if self.public_internet {
            docker.args(["--network", "bridge"]);
        } else {
            docker.args(["--network", "none"]);
        }
        let command = command_for_guest(command, &self.workspace, guest_workspace);
        let output = docker
            .arg("--mount")
            .arg(format!(
                "type=bind,src={},dst=/workspace",
                self.workspace.display()
            ))
            .arg("--workdir")
            .arg("/workspace")
            .arg(&self.image)
            .args(["/bin/sh", "-lc", &command])
            .output()
            .await
            .context("could not start Docker bash sandbox")?;
        Ok(SandboxOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(1),
        })
    }

    async fn shutdown(&self) {}
}

fn command_for_guest(command: &str, host_workspace: &Path, guest_workspace: &str) -> String {
    command.replace(host_workspace.to_string_lossy().as_ref(), guest_workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn sandbox_commands_translate_the_host_workspace_to_the_guest_mount() {
        let host = Path::new("/private/tmp/a3s bench/workspace");
        assert_eq!(
            command_for_guest(
                "ls '/private/tmp/a3s bench/workspace' && cat '/private/tmp/a3s bench/workspace/answer.txt'",
                host,
                "/workspace",
            ),
            "ls '/workspace' && cat '/workspace/answer.txt'"
        );
        assert_eq!(
            command_for_guest("printf 'workspace'", host, "/workspace"),
            "printf 'workspace'"
        );
    }

    #[test]
    fn workspace_contract_maps_the_extracted_source_directory_to_the_root() {
        let contract =
            workspace_contract(Some("/home/workspace/juliet-static-analyzer/agent-start"));
        assert!(contract.contains("already the `agent-start` directory"));
        assert!(contract.contains("use `path` with file tools"));
        assert!(contract.contains("public, read-only task fixtures"));
        assert!(contract.contains("all deliverable writes must stay inside `/workspace`"));
    }

    #[test]
    fn bundled_candidate_keeps_current_code_capabilities_enabled() {
        let workspace = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn BashSandbox> = Arc::new(DockerBashSandbox {
            image: "unused:test".into(),
            platform: None,
            workspace: workspace.path().to_path_buf(),
            game_network: None,
            public_internet: false,
        });
        let options = candidate_session_options("openai/fake", workspace.path(), sandbox, 64);

        assert_eq!(options.planning_mode, PlanningMode::Auto);
        assert_eq!(options.continuation_enabled, Some(true));
        assert_eq!(options.manual_delegation_enabled, Some(true));
        assert_eq!(options.max_tool_rounds, Some(64));
        assert!(
            !options
                .confirmation_policy
                .as_ref()
                .expect("benchmark candidate must install a confirmation manager")
                .enabled,
            "the isolated benchmark runtime must not pause for hidden HITL input"
        );
    }

    #[test]
    fn custom_openai_provider_edits_workspace_without_os_login() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut response_index = 0;
            while response_index < 2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
                let body_start = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                let request_body: serde_json::Value =
                    serde_json::from_slice(&request[body_start..]).unwrap();
                if request_body.get("stream").and_then(|value| value.as_bool()) == Some(true) {
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
                                .is_some_and(|content| {
                                    content.contains("You are a pre-analysis assistant")
                                })
                        })
                    });
                let message = if is_pre_analysis {
                    serde_json::json!({
                        "role":"assistant",
                        "content": serde_json::json!({
                            "intent": "GeneralPurpose",
                            "requires_planning": false,
                            "goal": {
                                "description": "Write 42 to answer.txt.",
                                "success_criteria": ["answer.txt contains 42"]
                            },
                            "execution_plan": {
                                "complexity": "Simple",
                                "steps": [{
                                    "id": "step-1",
                                    "description": "Update answer.txt",
                                    "tool": "write",
                                    "dependencies": [],
                                    "success_criteria": "answer.txt contains 42"
                                }],
                                "required_tools": ["write"]
                            },
                            "optimized_input": "Write 42 to answer.txt."
                        }).to_string()
                    })
                } else if response_index == 0 {
                    serde_json::json!({
                        "role":"assistant",
                        "content":null,
                        "tool_calls":[{
                            "id":"call_1",
                            "type":"function",
                            "function":{
                                "name":"write",
                                "arguments":"{\"file_path\":\"answer.txt\",\"content\":\"42\\n\"}"
                            }
                        }]
                    })
                } else {
                    serde_json::json!({"role":"assistant","content":"Completed and verified."})
                };
                let body = serde_json::to_vec(&serde_json::json!({
                    "id":"chatcmpl-test",
                    "object":"chat.completion",
                    "created":0,
                    "model":"fake",
                    "choices":[{"index":0,"message":message,"finish_reason":if response_index == 0 {"tool_calls"} else {"stop"}}],
                    "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
                }))
                .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                if !is_pre_analysis {
                    response_index += 1;
                }
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.acl");
        std::fs::write(
            &config,
            format!(
                "default_model = \"openai/unconfigured\"\nbench {{ judge_model = \"openai/fake\" }}\nproviders \"openai\" {{\n  api_key = \"test\"\n  base_url = \"http://{address}\"\n  models \"fake\" {{ name = \"Fake\" }}\n}}\n"
            ),
        )
        .unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("answer.txt"), "0\n").unwrap();
        let execution = execute(ModelCandidateRequest {
            config_path: &config,
            model: "openai/fake",
            task_prompt: "Write 42 to answer.txt.",
            candidate_instructions: "Follow the benchmark task.",
            workspace: &workspace,
            workspace_source_path: None,
            work_image: "alpine:3.20",
            work_platform: None,
            game_network: None,
            public_internet: false,
            timeout_sec: 30,
            max_tool_rounds: 32,
        })
        .unwrap();
        server.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("answer.txt")).unwrap(),
            "42\n"
        );
        assert_eq!(execution.total_tokens, 4);
        assert_eq!(execution.tool_calls_count, 1);
    }

    #[test]
    #[ignore = "requires Docker and the linux/amd64 imported game Judge image"]
    fn docker_bash_sandbox_reaches_only_the_game_network() {
        let task = crate::task::load_local(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("builtin/tasks/anchorhead_text_adventure"),
        )
        .unwrap();
        let source = task.legacy_judge.as_ref().unwrap();
        let state = tempfile::tempdir().unwrap();
        let game = crate::game_judge::GameSession::start(source, state.path()).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let sandbox = DockerBashSandbox {
            image: "python:3.12-alpine".into(),
            platform: None,
            workspace: workspace.path().to_path_buf(),
            game_network: Some((game.network().into(), game.url())),
            public_internet: false,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let output = runtime
            .block_on(sandbox.exec_command(
                "python -c \"import os,urllib.request;print(urllib.request.urlopen(os.environ['GAME_SERVER_URL']+'/health').read().decode())\"",
                "/workspace",
            ))
            .unwrap();
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(output.stdout.contains("true"));
    }
}
