use crate::command::AgentCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditMetadata {
    pub run_id: String,
    pub requested_by: String,
    pub command: String,
    pub agent: Option<String>,
    pub model: Option<String>,
}

impl AuditMetadata {
    pub fn from_command(
        run_id: impl Into<String>,
        requested_by: impl Into<String>,
        command: &AgentCommand,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            requested_by: requested_by.into(),
            command: command.raw.clone(),
            agent: command.agent.clone(),
            model: command.model.clone(),
        }
    }

    pub fn commit_trailers(&self) -> String {
        let mut trailers = vec![
            format!("Agent-Run: {}", self.run_id),
            format!("Requested-By: {}", self.requested_by),
            format!("Command: {}", self.command),
        ];

        if let Some(agent) = &self.agent {
            trailers.push(format!("Agent: {agent}"));
        }

        if let Some(model) = &self.model {
            trailers.push(format!("Model: {model}"));
        }

        trailers.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_commit_trailers() {
        let command = AgentCommand {
            agent: Some("coder".to_string()),
            model: Some("local".to_string()),
            task: "fix tests".to_string(),
            raw: "/agent:coder:local fix tests".to_string(),
            line_number: 1,
            command_index: 0,
        };

        let trailers =
            AuditMetadata::from_command("kiln_123", "@alice", &command).commit_trailers();

        assert!(trailers.contains("Agent-Run: kiln_123"));
        assert!(trailers.contains("Requested-By: @alice"));
        assert!(trailers.contains("Command: /agent:coder:local fix tests"));
        assert!(trailers.contains("Agent: coder"));
        assert!(trailers.contains("Model: local"));
    }
}
