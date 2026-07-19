use crate::{asset, lock, state_fs, task};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

pub struct RunOptions {
    pub task: String,
    pub agent: String,
    pub model: Option<String>,
    pub json: bool,
    locked: bool,
}

pub struct LoadedRun {
    pub task: task::TaskInfo,
    pub candidate: asset::LocalAssetPackage,
    pub judge: asset::LocalAssetPackage,
    pub judge_model: Option<String>,
    pub model: Option<String>,
    pub resolved_images: BTreeMap<String, String>,
    pub task_lock_digest: String,
    pub candidate_lock_digest: String,
}

impl RunOptions {
    pub fn parse(args: &[String]) -> Result<Self> {
        anyhow::ensure!(!args.is_empty(), "run requires one Task reference");
        let mut agent = None;
        let mut model = None;
        let mut json = false;
        let mut locked = false;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--agent" if agent.is_none() && index + 1 < args.len() => {
                    agent = Some(args[index + 1].clone());
                    index += 2;
                }
                "--model" if model.is_none() && index + 1 < args.len() => {
                    model = Some(args[index + 1].clone());
                    index += 2;
                }
                "--json" if !json => {
                    json = true;
                    index += 1;
                }
                "--locked" if !locked => {
                    locked = true;
                    index += 1;
                }
                value => anyhow::bail!("invalid or duplicate run option {value:?}"),
            }
        }
        Ok(Self {
            task: args[0].clone(),
            agent: agent.ok_or_else(|| anyhow::anyhow!("run requires exactly one --agent"))?,
            model,
            json,
            locked,
        })
    }

    pub fn load(
        &self,
        state_root: &Path,
        run_id: &str,
        judge_model: Option<String>,
        runtime_provider: &str,
    ) -> Result<LoadedRun> {
        if self.locked {
            anyhow::ensure!(
                self.model.is_none(),
                "--model cannot alter a locked Candidate"
            );
            return load_locks(Path::new(&self.task), Path::new(&self.agent), state_root);
        }
        let task_source = crate::catalog::resolve_task_reference(&self.task)?;
        let locks = state_root.join("locks");
        state_fs::secure_directory(&locks)?;
        let task_lock = locks.join(format!("{run_id}.task-lock.json"));
        let candidate_lock = locks.join(format!("{run_id}.candidate-lock.json"));
        lock::create_task_with_provider(
            &task_source,
            judge_model,
            state_root,
            &task_lock,
            runtime_provider,
        )?;
        lock::create_candidate(&self.agent, self.model.clone(), state_root, &candidate_lock)?;
        load_locks(&task_lock, &candidate_lock, state_root)
    }
}

fn load_locks(task_lock: &Path, candidate_lock: &Path, state_root: &Path) -> Result<LoadedRun> {
    let locked_task = lock::load_task(task_lock, state_root)?;
    let (candidate_lock, candidate_artifact) = lock::load_candidate(candidate_lock, state_root)?;
    Ok(LoadedRun {
        task: task::load_local(&locked_task.task_artifact)?,
        candidate: asset::load_local(&candidate_artifact)?,
        judge: asset::load_local(&locked_task.judge_artifact)?,
        judge_model: locked_task.lock.judge_model.clone(),
        model: candidate_lock.model,
        resolved_images: locked_task.lock.resolved_images,
        task_lock_digest: locked_task.lock.lock_digest,
        candidate_lock_digest: candidate_lock.lock_digest,
    })
}

#[cfg(test)]
mod tests;
