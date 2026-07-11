use crate::state_fs;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStage {
    Planned,
    RuntimeReady,
    InputsResolved,
    CandidateRunning,
    CandidateCompleted,
    Judging,
    Completed,
    Failed,
}

impl RunStage {
    fn rank(self) -> u8 {
        match self {
            Self::Planned => 0,
            Self::RuntimeReady => 1,
            Self::InputsResolved => 2,
            Self::CandidateRunning => 3,
            Self::CandidateCompleted => 4,
            Self::Judging => 5,
            Self::Completed | Self::Failed => 6,
        }
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl std::fmt::Display for RunStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Planned => "planned",
            Self::RuntimeReady => "runtime_ready",
            Self::InputsResolved => "inputs_resolved",
            Self::CandidateRunning => "candidate_running",
            Self::CandidateCompleted => "candidate_completed",
            Self::Judging => "judging",
            Self::Completed => "completed",
            Self::Failed => "failed",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunJournal {
    pub schema: String,
    pub run_id: String,
    pub task_reference: String,
    pub agent_reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_lock_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_lock_digest: Option<String>,
    pub stage: RunStage,
    pub updated_at_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl RunJournal {
    pub fn begin(state_root: &Path, task_reference: &str, agent_reference: &str) -> Result<Self> {
        let root = state_root.join("runs");
        state_fs::secure_directory(&root)?;
        let now = epoch_millis()?;
        let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let run_id = format!("local-{now}-{}-{sequence}", std::process::id());
        let mut journal = Self {
            schema: "a3s.bench.run-journal.v3".into(),
            path: root.join(format!("{run_id}.json")),
            run_id,
            task_reference: task_reference.into(),
            agent_reference: agent_reference.into(),
            task_lock_digest: None,
            candidate_lock_digest: None,
            stage: RunStage::Planned,
            updated_at_ms: now,
            result_path: None,
            result_digest: None,
            error: None,
        };
        journal.persist()?;
        Ok(journal)
    }

    pub fn advance(&mut self, stage: RunStage) -> Result<()> {
        anyhow::ensure!(
            !self.stage.terminal(),
            "terminal run journal cannot advance"
        );
        anyhow::ensure!(
            !stage.terminal() && stage.rank() == self.stage.rank() + 1,
            "invalid run journal transition from {:?} to {:?}",
            self.stage,
            stage
        );
        self.stage = stage;
        self.updated_at_ms = epoch_millis()?;
        self.persist()
    }

    pub fn bind_locks(&mut self, task_digest: &str, candidate_digest: &str) -> Result<()> {
        anyhow::ensure!(
            self.stage == RunStage::RuntimeReady,
            "run locks can only be bound after Runtime readiness"
        );
        anyhow::ensure!(
            self.task_lock_digest.is_none() && self.candidate_lock_digest.is_none(),
            "run locks are already bound"
        );
        crate::lock_identity::validate_digest(task_digest)?;
        crate::lock_identity::validate_digest(candidate_digest)?;
        self.task_lock_digest = Some(task_digest.into());
        self.candidate_lock_digest = Some(candidate_digest.into());
        self.updated_at_ms = epoch_millis()?;
        self.persist()
    }

    pub fn complete(&mut self, result_path: &Path, result_digest: &str) -> Result<()> {
        anyhow::ensure!(self.stage == RunStage::Judging, "run is not being judged");
        crate::lock_identity::validate_digest(result_digest)?;
        self.stage = RunStage::Completed;
        self.updated_at_ms = epoch_millis()?;
        self.result_path = Some(result_path.to_path_buf());
        self.result_digest = Some(result_digest.into());
        self.persist()
    }

    pub fn fail(&mut self, error: &anyhow::Error) -> Result<()> {
        if self.stage.terminal() {
            return Ok(());
        }
        self.stage = RunStage::Failed;
        self.updated_at_ms = epoch_millis()?;
        self.error = Some(format!("{error:#}"));
        self.persist()
    }

    pub fn load(state_root: &Path, run_id: &str) -> Result<Self> {
        validate_run_id(run_id)?;
        let path = state_root.join("runs").join(format!("{run_id}.json"));
        let mut journal: Self =
            serde_json::from_slice(&state_fs::read_regular_file(&path, "run journal")?)?;
        anyhow::ensure!(
            journal.schema == "a3s.bench.run-journal.v3",
            "unsupported run journal schema"
        );
        anyhow::ensure!(journal.run_id == run_id, "run journal identity mismatch");
        anyhow::ensure!(
            journal.task_lock_digest.is_some() == journal.candidate_lock_digest.is_some(),
            "run journal lock binding is incomplete"
        );
        if let Some(task_digest) = &journal.task_lock_digest {
            crate::lock_identity::validate_digest(task_digest)?;
        }
        if let Some(candidate_digest) = &journal.candidate_lock_digest {
            crate::lock_identity::validate_digest(candidate_digest)?;
        }
        anyhow::ensure!(
            journal.stage == RunStage::Failed
                || journal.stage.rank() < RunStage::InputsResolved.rank()
                || journal.task_lock_digest.is_some(),
            "resolved run journal has no lock binding"
        );
        anyhow::ensure!(
            (journal.stage == RunStage::Completed) == journal.result_path.is_some(),
            "run journal result binding is invalid"
        );
        anyhow::ensure!(
            (journal.stage == RunStage::Completed) == journal.result_digest.is_some(),
            "run journal result digest binding is invalid"
        );
        if let Some(result_digest) = &journal.result_digest {
            crate::lock_identity::validate_digest(result_digest)?;
        }
        anyhow::ensure!(
            (journal.stage == RunStage::Failed) == journal.error.is_some(),
            "run journal failure binding is invalid"
        );
        journal.path = path;
        Ok(journal)
    }

    pub fn public_projection(&self) -> serde_json::Value {
        serde_json::json!({
            "status":self.stage,
            "run_id":self.run_id,
            "task_reference":self.task_reference,
        })
    }

    fn persist(&mut self) -> Result<()> {
        state_fs::secure_atomic_write(&self.path, &serde_json::to_vec_pretty(self)?)
    }
}

pub fn validate_run_id(run_id: &str) -> Result<()> {
    anyhow::ensure!(
        run_id.starts_with("local-")
            && run_id.len() <= 128
            && run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid run ID"
    );
    Ok(())
}

fn epoch_millis() -> Result<u128> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_is_durable_and_transitions_are_closed() {
        let state = tempfile::tempdir().unwrap();
        let mut journal = RunJournal::begin(state.path(), "./task", "./agent").unwrap();
        journal.advance(RunStage::RuntimeReady).unwrap();
        journal
            .bind_locks(
                &format!("sha256:{}", "a".repeat(64)),
                &format!("sha256:{}", "b".repeat(64)),
            )
            .unwrap();
        journal.advance(RunStage::InputsResolved).unwrap();
        assert!(journal.advance(RunStage::CandidateCompleted).is_err());
        journal.advance(RunStage::CandidateRunning).unwrap();
        let bytes = std::fs::read(
            state
                .path()
                .join("runs")
                .join(format!("{}.json", journal.run_id)),
        )
        .unwrap();
        let persisted: RunJournal = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted.stage, RunStage::CandidateRunning);
        assert_eq!(persisted.schema, "a3s.bench.run-journal.v3");
    }

    #[test]
    fn failure_is_terminal() {
        let state = tempfile::tempdir().unwrap();
        let mut journal = RunJournal::begin(state.path(), "task", "agent").unwrap();
        journal
            .fail(&anyhow::anyhow!("runtime unavailable"))
            .unwrap();
        assert_eq!(journal.stage, RunStage::Failed);
        assert_eq!(journal.error.as_deref(), Some("runtime unavailable"));
        assert!(journal.advance(RunStage::RuntimeReady).is_err());
    }

    #[test]
    fn load_rejects_identity_substitution_and_unknown_fields() {
        let state = tempfile::tempdir().unwrap();
        let journal = RunJournal::begin(state.path(), "task", "agent").unwrap();
        assert_eq!(
            RunJournal::load(state.path(), &journal.run_id)
                .unwrap()
                .stage,
            RunStage::Planned
        );
        assert!(RunJournal::load(state.path(), "../journal").is_err());

        let path = state
            .path()
            .join("runs")
            .join(format!("{}.json", journal.run_id));
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        state_fs::secure_atomic_write(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(RunJournal::load(state.path(), &journal.run_id).is_err());
    }
}
