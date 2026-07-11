use crate::result_record::LocalResultRecord;
use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct LocalResultIdentity<'a> {
    schema: &'a str,
    governance_status: &'a str,
    run_id: &'a str,
    task_id: &'a str,
    task_lock_digest: &'a str,
    agent: &'a str,
    candidate_lock_digest: &'a str,
    agent_identity: &'a str,
    judge_identity: &'a str,
    runtime_provider: &'a str,
    model: &'a Option<String>,
    model_usage: &'a Option<crate::model_candidate::ModelExecution>,
    primary_metric: &'a str,
    score: &'a str,
    judge_result: &'a crate::runtime::JudgeResult,
}

pub fn calculate(value: &LocalResultRecord) -> Result<String> {
    let identity = LocalResultIdentity {
        schema: &value.schema,
        governance_status: &value.governance_status,
        run_id: &value.run_id,
        task_id: &value.task_id,
        task_lock_digest: &value.task_lock_digest,
        agent: &value.agent,
        candidate_lock_digest: &value.candidate_lock_digest,
        agent_identity: &value.agent_identity,
        judge_identity: &value.judge_identity,
        runtime_provider: &value.runtime_provider,
        model: &value.model,
        model_usage: &value.model_usage,
        primary_metric: &value.primary_metric,
        score: &value.score,
        judge_result: &value.judge_result,
    };
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&identity)?)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_covers_non_score_evidence() {
        let mut value = LocalResultRecord {
            schema: "a3s.bench.local-result.v4".into(),
            result_digest: String::new(),
            governance_status: "local_unofficial".into(),
            run_id: "local-1".into(),
            task_id: "task".into(),
            task_lock_digest: format!("sha256:{}", "a".repeat(64)),
            agent: "agent".into(),
            candidate_lock_digest: format!("sha256:{}", "b".repeat(64)),
            agent_identity: "agent-id".into(),
            judge_identity: "judge-id".into(),
            runtime_provider: "docker".into(),
            model: None,
            model_usage: None,
            primary_metric: "score".into(),
            score: "1".into(),
            judge_result: crate::runtime::JudgeResult {
                schema: "bench.judge.result.v1".into(),
                solution_verdict: "valid".into(),
                metrics: serde_json::from_value(serde_json::json!({"score":"1"})).unwrap(),
                diagnostics: serde_json::json!({}),
            },
        };
        let first = calculate(&value).unwrap();
        value.runtime_provider = "substituted".into();
        assert_ne!(first, calculate(&value).unwrap());
    }
}
