use a3s_code_core::sandbox::{BashSandbox, SandboxOutput};
use a3s_code_core::{Agent, SessionOptions, WorkspaceServices};
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
    pub work_image: &'a str,
    pub work_platform: Option<&'a str>,
    pub game_network: Option<(&'a str, &'a str)>,
    pub public_internet: bool,
}

pub fn execute(request: ModelCandidateRequest<'_>) -> Result<ModelExecution> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(execute_async(request))
}

async fn execute_async(request: ModelCandidateRequest<'_>) -> Result<ModelExecution> {
    let config = request
        .config_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("config.acl path is not UTF-8"))?;
    let agent = Agent::new(config)
        .await
        .context("could not initialize model Candidate from config.acl")?;
    let sandbox = Arc::new(DockerBashSandbox {
        image: request.work_image.to_owned(),
        platform: request.work_platform.map(str::to_owned),
        workspace: request.workspace.canonicalize()?,
        game_network: request
            .game_network
            .map(|(network, url)| (network.to_owned(), url.to_owned())),
        public_internet: request.public_internet,
    });
    let options = SessionOptions::new()
        .with_model(request.model)
        .with_workspace_backend(WorkspaceServices::local(request.workspace))
        .with_sandbox_handle(sandbox)
        .with_confirmation_policy(a3s_code_core::hitl::ConfirmationPolicy::default())
        .with_max_tool_rounds(25)
        .with_planning(false)
        .with_continuation(false)
        .with_manual_delegation_enabled(false);
    let session = agent
        .session(request.workspace.display().to_string(), Some(options))
        .context("could not create model Candidate session")?;
    let prompt = format!(
        "{}\n\n# Benchmark task\n\n{}\n\nWork only inside the supplied workspace. Complete the task and verify the result.",
        request.candidate_instructions,
        request.task_prompt
    );
    let result = session
        .send(&prompt, None)
        .await
        .context("model Candidate execution failed")?;
    session.close().await;
    Ok(ModelExecution {
        prompt_tokens: result.usage.prompt_tokens,
        completion_tokens: result.usage.completion_tokens,
        total_tokens: result.usage.total_tokens,
        cache_read_tokens: result.usage.cache_read_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        tool_calls_count: result.tool_calls_count,
    })
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
            "--pids-limit",
            "256",
            "--memory",
            "2g",
            "--cpus",
            "2",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=64m",
        ]);
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
        let output = docker
            .arg("--mount")
            .arg(format!(
                "type=bind,src={},dst=/workspace",
                self.workspace.display()
            ))
            .arg("--workdir")
            .arg("/workspace")
            .arg(&self.image)
            .args(["/bin/sh", "-lc", command])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

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
                let message = if response_index == 0 {
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
                response_index += 1;
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.acl");
        std::fs::write(
            &config,
            format!(
                "default_model = \"openai/fake\"\nbench {{ judge_model = \"openai/fake\" }}\nproviders \"openai\" {{\n  api_key = \"test\"\n  base_url = \"http://{address}\"\n  models \"fake\" {{ name = \"Fake\" }}\n}}\n"
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
            work_image: "alpine:3.20",
            work_platform: None,
            game_network: None,
            public_internet: false,
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
