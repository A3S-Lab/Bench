use crate::result_record::LocalResultRecord;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairOutcome {
    BaselineWin,
    Tie,
    CandidateWin,
}

impl std::fmt::Display for PairOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BaselineWin => "baseline_win",
            Self::Tie => "tie",
            Self::CandidateWin => "candidate_win",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateDescriptor {
    pub identity: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairComparison {
    pub task_id: String,
    pub task_lock_digest: String,
    pub baseline_run_id: String,
    pub candidate_run_id: String,
    pub primary_metric: String,
    pub baseline_score: String,
    pub candidate_score: String,
    pub outcome: PairOutcome,
    pub baseline_timed_out: bool,
    pub candidate_timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComparisonSummary {
    pub schema: String,
    pub governance_status: String,
    pub baseline: CandidateDescriptor,
    pub candidate: CandidateDescriptor,
    pub pair_count: usize,
    pub baseline_wins: usize,
    pub ties: usize,
    pub candidate_wins: usize,
    pub baseline_timeouts: usize,
    pub candidate_timeouts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_total_tokens: Option<u64>,
    pub pairs: Vec<PairComparison>,
}

pub fn compare_pairs(
    pairs: &[(LocalResultRecord, LocalResultRecord)],
) -> Result<ComparisonSummary> {
    anyhow::ensure!(
        !pairs.is_empty(),
        "comparison requires at least one result pair"
    );
    let baseline = descriptor(&pairs[0].0);
    let candidate = descriptor(&pairs[0].1);
    let mut comparisons = Vec::with_capacity(pairs.len());
    let mut baseline_wins = 0;
    let mut ties = 0;
    let mut candidate_wins = 0;
    let mut baseline_timeouts = 0;
    let mut candidate_timeouts = 0;
    let mut baseline_tokens = TokenAggregate::default();
    let mut candidate_tokens = TokenAggregate::default();

    for (baseline_result, candidate_result) in pairs {
        anyhow::ensure!(
            descriptor(baseline_result) == baseline,
            "baseline results do not identify one Candidate and model"
        );
        anyhow::ensure!(
            descriptor(candidate_result) == candidate,
            "candidate results do not identify one Candidate and model"
        );
        anyhow::ensure!(
            baseline_result.task_lock_digest == candidate_result.task_lock_digest,
            "result pair {} / {} does not bind the same Task lock",
            baseline_result.run_id,
            candidate_result.run_id
        );
        anyhow::ensure!(
            baseline_result.task_id == candidate_result.task_id
                && baseline_result.primary_metric == candidate_result.primary_metric,
            "result pair {} / {} has inconsistent Task metadata",
            baseline_result.run_id,
            candidate_result.run_id
        );
        let baseline_score = parse_score(&baseline_result.score)?;
        let candidate_score = parse_score(&candidate_result.score)?;
        let outcome = if baseline_score > candidate_score {
            baseline_wins += 1;
            PairOutcome::BaselineWin
        } else if baseline_score < candidate_score {
            candidate_wins += 1;
            PairOutcome::CandidateWin
        } else {
            ties += 1;
            PairOutcome::Tie
        };
        let baseline_timed_out = timed_out(baseline_result);
        let candidate_timed_out = timed_out(candidate_result);
        baseline_timeouts += usize::from(baseline_timed_out);
        candidate_timeouts += usize::from(candidate_timed_out);
        baseline_tokens.observe(baseline_result);
        candidate_tokens.observe(candidate_result);
        comparisons.push(PairComparison {
            task_id: baseline_result.task_id.clone(),
            task_lock_digest: baseline_result.task_lock_digest.clone(),
            baseline_run_id: baseline_result.run_id.clone(),
            candidate_run_id: candidate_result.run_id.clone(),
            primary_metric: baseline_result.primary_metric.clone(),
            baseline_score: baseline_result.score.clone(),
            candidate_score: candidate_result.score.clone(),
            outcome,
            baseline_timed_out,
            candidate_timed_out,
        });
    }

    Ok(ComparisonSummary {
        schema: "a3s.bench.comparison.v1".into(),
        governance_status: "local_unofficial".into(),
        baseline,
        candidate,
        pair_count: comparisons.len(),
        baseline_wins,
        ties,
        candidate_wins,
        baseline_timeouts,
        candidate_timeouts,
        baseline_total_tokens: baseline_tokens.finish(),
        candidate_total_tokens: candidate_tokens.finish(),
        pairs: comparisons,
    })
}

fn descriptor(record: &LocalResultRecord) -> CandidateDescriptor {
    CandidateDescriptor {
        identity: record.agent_identity.clone(),
        model: record.model.clone(),
    }
}

fn parse_score(value: &str) -> Result<f64> {
    let score = value.parse::<f64>()?;
    anyhow::ensure!(score.is_finite(), "result score is not finite");
    Ok(score)
}

fn timed_out(record: &LocalResultRecord) -> bool {
    record
        .candidate_execution
        .as_ref()
        .is_some_and(|execution| execution.is_timed_out())
}

#[derive(Default)]
struct TokenAggregate {
    saw_model: bool,
    complete: bool,
    total: Option<u64>,
}

impl TokenAggregate {
    fn observe(&mut self, record: &LocalResultRecord) {
        if record.model.is_none() {
            return;
        }
        if !self.saw_model {
            self.complete = true;
        }
        self.saw_model = true;
        match record.model_usage.as_ref() {
            Some(usage) if self.complete => {
                self.total = u64::try_from(usage.total_tokens)
                    .ok()
                    .and_then(|tokens| self.total.unwrap_or_default().checked_add(tokens));
                self.complete = self.total.is_some();
            }
            _ => {
                self.complete = false;
                self.total = None;
            }
        }
    }

    fn finish(self) -> Option<u64> {
        (self.saw_model && self.complete)
            .then_some(self.total)
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result_record::{CandidateExecution, CandidateExecutionStatus};
    use crate::runtime::JudgeResult;

    fn result(run: &str, identity: &str, task_digest: &str, score: &str) -> LocalResultRecord {
        LocalResultRecord {
            schema: "a3s.bench.local-result.v5".into(),
            result_digest: format!("sha256:{}", "c".repeat(64)),
            governance_status: "local_unofficial".into(),
            run_id: run.into(),
            task_id: "edit-task".into(),
            task_lock_digest: task_digest.into(),
            agent: identity.into(),
            candidate_lock_digest: format!("sha256:{}", "b".repeat(64)),
            agent_identity: identity.into(),
            judge_identity: "judge".into(),
            runtime_provider: "docker".into(),
            model: None,
            candidate_execution: Some(CandidateExecution::completed()),
            model_usage: None,
            primary_metric: "score".into(),
            score: score.into(),
            judge_result: JudgeResult {
                schema: "bench.judge.result.v1".into(),
                solution_verdict: "valid".into(),
                metrics: serde_json::from_value(serde_json::json!({"score": score})).unwrap(),
                diagnostics: serde_json::json!({}),
            },
        }
    }

    #[test]
    fn aggregates_paired_wins_ties_and_timeouts() {
        let digest_a = format!("sha256:{}", "a".repeat(64));
        let digest_b = format!("sha256:{}", "d".repeat(64));
        let mut timed_out = result("candidate-2", "candidate", &digest_b, "0");
        timed_out.candidate_execution = Some(CandidateExecution {
            status: CandidateExecutionStatus::TimedOut,
            timeout_sec: Some(300),
        });
        let summary = compare_pairs(&[
            (
                result("baseline-1", "baseline", &digest_a, "0.5"),
                result("candidate-1", "candidate", &digest_a, "1"),
            ),
            (result("baseline-2", "baseline", &digest_b, "0"), timed_out),
        ])
        .unwrap();
        assert_eq!(summary.pair_count, 2);
        assert_eq!(summary.candidate_wins, 1);
        assert_eq!(summary.ties, 1);
        assert_eq!(summary.baseline_wins, 0);
        assert_eq!(summary.candidate_timeouts, 1);
        assert_eq!(summary.pairs[0].outcome, PairOutcome::CandidateWin);
    }

    #[test]
    fn rejects_unpaired_tasks_and_mixed_candidates() {
        let digest_a = format!("sha256:{}", "a".repeat(64));
        let digest_b = format!("sha256:{}", "d".repeat(64));
        assert!(compare_pairs(&[(
            result("baseline", "baseline", &digest_a, "1"),
            result("candidate", "candidate", &digest_b, "1"),
        )])
        .is_err());
        assert!(compare_pairs(&[
            (
                result("baseline-1", "baseline", &digest_a, "1"),
                result("candidate-1", "candidate", &digest_a, "1"),
            ),
            (
                result("baseline-2", "other", &digest_a, "1"),
                result("candidate-2", "candidate", &digest_a, "1"),
            ),
        ])
        .is_err());
    }
}
