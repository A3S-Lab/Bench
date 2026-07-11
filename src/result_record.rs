use crate::model_candidate::ModelExecution;
use crate::runtime::{canonical_decimal, JudgeResult};
use crate::{run_journal, state_fs};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalResultRecord {
    pub schema: String,
    pub result_digest: String,
    pub governance_status: String,
    pub run_id: String,
    pub task_id: String,
    pub task_lock_digest: String,
    pub agent: String,
    pub candidate_lock_digest: String,
    pub agent_identity: String,
    pub judge_identity: String,
    pub runtime_provider: String,
    pub model: Option<String>,
    pub model_usage: Option<ModelExecution>,
    pub primary_metric: String,
    pub score: String,
    pub judge_result: JudgeResult,
}

pub struct NewLocalResult<'a> {
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub task_lock_digest: &'a str,
    pub agent: &'a str,
    pub candidate_lock_digest: &'a str,
    pub agent_identity: &'a str,
    pub judge_identity: &'a str,
    pub runtime_provider: &'a str,
    pub model: Option<&'a str>,
    pub model_usage: Option<&'a ModelExecution>,
    pub primary_metric: &'a str,
    pub score: &'a str,
    pub judge_result: &'a JudgeResult,
}

impl LocalResultRecord {
    pub fn save(state_root: &Path, input: NewLocalResult<'_>) -> Result<(Self, PathBuf)> {
        let mut record = Self {
            schema: "a3s.bench.local-result.v4".into(),
            result_digest: String::new(),
            governance_status: "local_unofficial".into(),
            run_id: input.run_id.into(),
            task_id: input.task_id.into(),
            task_lock_digest: input.task_lock_digest.into(),
            agent: input.agent.into(),
            candidate_lock_digest: input.candidate_lock_digest.into(),
            agent_identity: input.agent_identity.into(),
            judge_identity: input.judge_identity.into(),
            runtime_provider: input.runtime_provider.into(),
            model: input.model.map(str::to_owned),
            model_usage: input.model_usage.cloned(),
            primary_metric: input.primary_metric.into(),
            score: input.score.into(),
            judge_result: input.judge_result.clone(),
        };
        record.result_digest = crate::result_identity::calculate(&record)?;
        record.validate(&record.run_id)?;
        let root = state_root.join("results");
        state_fs::secure_directory(&root)?;
        let path = root.join(format!("{}.json", record.run_id));
        state_fs::secure_atomic_write(&path, &serde_json::to_vec_pretty(&record)?)?;
        state_fs::secure_atomic_write(
            &root.join("latest"),
            format!("{}\n", record.run_id).as_bytes(),
        )?;
        Ok((record, path))
    }

    pub fn load(state_root: &Path, run_id: &str) -> Result<Option<Self>> {
        run_journal::validate_run_id(run_id)?;
        let path = state_root.join("results").join(format!("{run_id}.json"));
        let Some(bytes) = state_fs::read_optional_regular_file(&path, "local result")? else {
            return Ok(None);
        };
        let record: Self = serde_json::from_slice(&bytes)?;
        record.validate(run_id)?;
        let journal = run_journal::RunJournal::load(state_root, run_id)?;
        anyhow::ensure!(
            journal.stage == run_journal::RunStage::Completed,
            "local result is not backed by a completed run journal"
        );
        anyhow::ensure!(
            journal.task_lock_digest.as_deref() == Some(record.task_lock_digest.as_str())
                && journal.candidate_lock_digest.as_deref()
                    == Some(record.candidate_lock_digest.as_str()),
            "local result lock binding does not match its run journal"
        );
        anyhow::ensure!(
            journal.result_digest.as_deref() == Some(record.result_digest.as_str()),
            "local result digest does not match its run journal"
        );
        anyhow::ensure!(
            journal.result_path.as_deref() == Some(path.as_path()),
            "local result path does not match its run journal"
        );
        Ok(Some(record))
    }

    pub fn latest_run_id(state_root: &Path) -> Result<String> {
        let bytes = state_fs::read_regular_file(
            &state_root.join("results/latest"),
            "latest result pointer",
        )?;
        let run_id = std::str::from_utf8(&bytes)?.trim().to_owned();
        run_journal::validate_run_id(&run_id)?;
        Ok(run_id)
    }

    pub fn public_projection(&self) -> serde_json::Value {
        serde_json::json!({
            "status": "completed",
            "governance_status": self.governance_status,
            "run_id": self.run_id,
            "task_id": self.task_id,
            "task_lock_digest": self.task_lock_digest,
            "candidate_lock_digest": self.candidate_lock_digest,
            "candidate_identity": self.agent_identity,
            "judge_identity": self.judge_identity,
            "runtime_provider": self.runtime_provider,
            "model": self.model,
            "model_usage": self.model_usage,
            "primary_metric": self.primary_metric,
            "score": self.score,
            "result_digest": self.result_digest,
        })
    }

    fn validate(&self, expected_run_id: &str) -> Result<()> {
        run_journal::validate_run_id(&self.run_id)?;
        anyhow::ensure!(
            self.schema == "a3s.bench.local-result.v4",
            "unsupported local result schema"
        );
        anyhow::ensure!(
            self.governance_status == "local_unofficial",
            "invalid local result governance status"
        );
        anyhow::ensure!(
            self.run_id == expected_run_id,
            "local result identity mismatch"
        );
        crate::lock_identity::validate_digest(&self.result_digest)?;
        anyhow::ensure!(
            crate::result_identity::calculate(self)? == self.result_digest,
            "local result semantic digest mismatch"
        );
        for (name, value) in [
            ("task_id", self.task_id.as_str()),
            ("agent", self.agent.as_str()),
            ("agent_identity", self.agent_identity.as_str()),
            ("judge_identity", self.judge_identity.as_str()),
            ("runtime_provider", self.runtime_provider.as_str()),
            ("primary_metric", self.primary_metric.as_str()),
        ] {
            anyhow::ensure!(!value.trim().is_empty(), "local result {name} is empty");
        }
        crate::lock_identity::validate_digest(&self.task_lock_digest)?;
        crate::lock_identity::validate_digest(&self.candidate_lock_digest)?;
        if let Some(model) = &self.model {
            anyhow::ensure!(!model.trim().is_empty(), "local result model is empty");
            anyhow::ensure!(
                self.model_usage.is_some(),
                "model-backed result has no usage"
            );
        } else {
            anyhow::ensure!(
                self.model_usage.is_none(),
                "model usage exists without a model"
            );
        }
        if let Some(usage) = &self.model_usage {
            anyhow::ensure!(
                usage.prompt_tokens.checked_add(usage.completion_tokens)
                    == Some(usage.total_tokens),
                "model token usage total is inconsistent"
            );
        }
        anyhow::ensure!(
            canonical_decimal(&self.score),
            "local result score is not canonical"
        );
        anyhow::ensure!(
            self.judge_result.schema == "bench.judge.result.v1",
            "invalid JudgeResult schema"
        );
        anyhow::ensure!(
            self.judge_result.solution_verdict == "valid",
            "invalid JudgeResult verdict"
        );
        anyhow::ensure!(
            self.judge_result
                .metrics
                .get(&self.primary_metric)
                .and_then(serde_json::Value::as_str)
                == Some(self.score.as_str()),
            "local result score does not match its primary Judge metric"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judge() -> JudgeResult {
        JudgeResult {
            schema: "bench.judge.result.v1".into(),
            solution_verdict: "valid".into(),
            metrics: serde_json::from_value(serde_json::json!({"score":"1"})).unwrap(),
            diagnostics: serde_json::json!({}),
        }
    }

    #[test]
    fn roundtrip_binds_score_to_primary_metric() {
        let state = tempfile::tempdir().unwrap();
        let judge = judge();
        let task_digest = format!("sha256:{}", "a".repeat(64));
        let candidate_digest = format!("sha256:{}", "b".repeat(64));
        let mut journal = run_journal::RunJournal::begin(state.path(), "task", "agent").unwrap();
        journal
            .advance(run_journal::RunStage::RuntimeReady)
            .unwrap();
        journal.bind_locks(&task_digest, &candidate_digest).unwrap();
        journal
            .advance(run_journal::RunStage::InputsResolved)
            .unwrap();
        journal
            .advance(run_journal::RunStage::CandidateRunning)
            .unwrap();
        journal
            .advance(run_journal::RunStage::CandidateCompleted)
            .unwrap();
        journal.advance(run_journal::RunStage::Judging).unwrap();
        let (saved, path) = LocalResultRecord::save(
            state.path(),
            NewLocalResult {
                run_id: &journal.run_id,
                task_id: "task",
                task_lock_digest: &task_digest,
                agent: "agent",
                candidate_lock_digest: &candidate_digest,
                agent_identity: "sha256:agent",
                judge_identity: "sha256:judge",
                runtime_provider: "docker",
                model: None,
                model_usage: None,
                primary_metric: "score",
                score: "1",
                judge_result: &judge,
            },
        )
        .unwrap();
        journal.complete(&path, &saved.result_digest).unwrap();
        let loaded = LocalResultRecord::load(state.path(), &saved.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.score, "1");
        assert_eq!(
            LocalResultRecord::latest_run_id(state.path()).unwrap(),
            saved.run_id
        );
        let mut substituted = saved;
        substituted.task_lock_digest = format!("sha256:{}", "c".repeat(64));
        state_fs::secure_atomic_write(&path, &serde_json::to_vec(&substituted).unwrap()).unwrap();
        assert!(LocalResultRecord::load(state.path(), &journal.run_id).is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_score_tampering() {
        let mut value = serde_json::json!({
            "schema":"a3s.bench.local-result.v4", "result_digest":format!("sha256:{}", "c".repeat(64)),
            "governance_status":"local_unofficial",
            "run_id":"local-1", "task_id":"task", "agent":"agent",
            "task_lock_digest":format!("sha256:{}", "a".repeat(64)),
            "candidate_lock_digest":format!("sha256:{}", "b".repeat(64)),
            "agent_identity":"agent-id", "judge_identity":"judge-id",
            "runtime_provider":"docker", "model":null, "model_usage":null,
            "primary_metric":"score", "score":"0",
            "judge_result":{"schema":"bench.judge.result.v1","solution_verdict":"valid","metrics":{"score":"1"},"diagnostics":{}}
        });
        let record: LocalResultRecord = serde_json::from_value(value.clone()).unwrap();
        assert!(record.validate("local-1").is_err());
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LocalResultRecord>(value).is_err());
    }

    #[test]
    fn public_projection_omits_private_diagnostics_and_source_reference() {
        let record: LocalResultRecord = serde_json::from_value(serde_json::json!({
            "schema":"a3s.bench.local-result.v4",
            "result_digest":format!("sha256:{}", "c".repeat(64)),
            "governance_status":"local_unofficial", "run_id":"local-1",
            "task_id":"task", "task_lock_digest":format!("sha256:{}", "a".repeat(64)),
            "agent":"./private/adapter", "candidate_lock_digest":format!("sha256:{}", "b".repeat(64)),
            "agent_identity":"candidate-id", "judge_identity":"judge-id",
            "runtime_provider":"docker", "model":null, "model_usage":null,
            "primary_metric":"score", "score":"1",
            "judge_result":{"schema":"bench.judge.result.v1","solution_verdict":"valid","metrics":{"score":"1"},"diagnostics":{"private":"secret"}}
        })).unwrap();
        let projection = record.public_projection();
        assert!(projection.get("agent").is_none());
        assert!(projection.get("judge_result").is_none());
        assert_eq!(projection["candidate_identity"], "candidate-id");
    }
}
