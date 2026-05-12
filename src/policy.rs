use crate::{command::AgentCommand, github::RepoPermission};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
}

impl PolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PolicyEngine;

impl PolicyEngine {
    pub fn evaluate_invocation(
        &self,
        permission: &RepoPermission,
        _command: &AgentCommand,
    ) -> PolicyDecision {
        if permission.can_invoke_agent() {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(
                "requester must have write, maintain, or admin permission".to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> AgentCommand {
        AgentCommand {
            agent: None,
            model: None,
            task: "ping".to_string(),
            raw: "/agent ping".to_string(),
            line_number: 1,
            command_index: 0,
        }
    }

    #[test]
    fn allows_maintainer_level_permissions() {
        let policy = PolicyEngine;

        assert!(policy
            .evaluate_invocation(&RepoPermission::Write, &command())
            .is_allowed());
        assert!(policy
            .evaluate_invocation(&RepoPermission::Maintain, &command())
            .is_allowed());
        assert!(policy
            .evaluate_invocation(&RepoPermission::Admin, &command())
            .is_allowed());
    }

    #[test]
    fn rejects_lower_permissions() {
        let policy = PolicyEngine;

        assert!(!policy
            .evaluate_invocation(&RepoPermission::Read, &command())
            .is_allowed());
        assert!(!policy
            .evaluate_invocation(&RepoPermission::Triage, &command())
            .is_allowed());
    }
}
