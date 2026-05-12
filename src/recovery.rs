use crate::domain::RunStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunObservation {
    pub run_id: String,
    pub status: RunStatus,
    pub age_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    RecoverMissingRun { run_id: String },
    MarkStaleRun { run_id: String },
}

pub fn recover_missing_run(run_id: impl Into<String>, run_exists: bool) -> Option<RecoveryAction> {
    if run_exists {
        None
    } else {
        Some(RecoveryAction::RecoverMissingRun {
            run_id: run_id.into(),
        })
    }
}

pub fn classify_stale_run(
    observation: &RunObservation,
    stale_after_seconds: u64,
) -> Option<RecoveryAction> {
    if matches!(observation.status, RunStatus::Completed | RunStatus::Failed) {
        return None;
    }

    if observation.age_seconds >= stale_after_seconds {
        Some(RecoveryAction::MarkStaleRun {
            run_id: observation.run_id.clone(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_missing_runs() {
        assert_eq!(
            recover_missing_run("kiln_123", false),
            Some(RecoveryAction::RecoverMissingRun {
                run_id: "kiln_123".to_string()
            })
        );
        assert_eq!(recover_missing_run("kiln_123", true), None);
    }

    #[test]
    fn marks_non_terminal_stale_runs() {
        let observation = RunObservation {
            run_id: "kiln_123".to_string(),
            status: RunStatus::Running,
            age_seconds: 600,
        };

        assert_eq!(
            classify_stale_run(&observation, 300),
            Some(RecoveryAction::MarkStaleRun {
                run_id: "kiln_123".to_string()
            })
        );
    }

    #[test]
    fn ignores_terminal_stale_runs() {
        let observation = RunObservation {
            run_id: "kiln_123".to_string(),
            status: RunStatus::Completed,
            age_seconds: 600,
        };

        assert_eq!(classify_stale_run(&observation, 300), None);
    }
}
