#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub task: String,
    pub raw: String,
    pub line_number: usize,
    pub command_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub line_number: usize,
    pub command_index: usize,
    pub raw: String,
    pub parsed: Result<AgentCommand, String>,
}

pub fn extract_commands(body: &str) -> Vec<CommandLine> {
    let mut commands = Vec::new();

    for (line_idx, line) in body.lines().enumerate() {
        if !is_agent_command_line(line) {
            continue;
        }

        let command_index = commands.len();
        let line_number = line_idx + 1;
        let raw = line.to_string();
        let parsed = parse_command_line(line, line_number, command_index);

        commands.push(CommandLine {
            line_number,
            command_index,
            raw,
            parsed,
        });
    }

    commands
}

fn is_agent_command_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("/agent") else {
        return false;
    };

    rest.is_empty()
        || rest
            .chars()
            .next()
            .is_some_and(|character| character == ':' || character.is_whitespace())
}

fn parse_command_line(
    line: &str,
    line_number: usize,
    command_index: usize,
) -> Result<AgentCommand, String> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let prefix = parts.next().unwrap_or_default();
    let task = parts.next().unwrap_or_default().trim();

    if task.is_empty() {
        return Err("command must include a task".to_string());
    }

    let (agent, model) = parse_prefix(prefix)?;

    Ok(AgentCommand {
        agent,
        model,
        task: task.to_string(),
        raw: line.to_string(),
        line_number,
        command_index,
    })
}

fn parse_prefix(prefix: &str) -> Result<(Option<String>, Option<String>), String> {
    if prefix == "/agent" {
        return Ok((None, None));
    }

    let Some(rest) = prefix.strip_prefix("/agent:") else {
        return Err(
            "command must start with `/agent`, `/agent:<agent>`, or `/agent:<agent>:<model>`"
                .to_string(),
        );
    };

    let fields = rest.split(':').collect::<Vec<_>>();
    match fields.as_slice() {
        [agent] if !agent.is_empty() => Ok((Some((*agent).to_string()), None)),
        [agent, model] if !agent.is_empty() && !model.is_empty() => {
            Ok((Some((*agent).to_string()), Some((*model).to_string())))
        }
        _ => Err("command must use `/agent:<agent>` or `/agent:<agent>:<model>`".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_line_start_commands_only() {
        let commands =
            extract_commands("/agent fix tests\ntext /agent ignored\n/agent:reviewer review");

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].parsed.as_ref().unwrap().agent, None);
        assert_eq!(commands[0].parsed.as_ref().unwrap().model, None);
        assert_eq!(
            commands[1].parsed.as_ref().unwrap().agent.as_deref(),
            Some("reviewer")
        );
        assert_eq!(commands[1].parsed.as_ref().unwrap().model, None);
    }

    #[test]
    fn preserves_opaque_agent_and_model_values() {
        let commands = extract_commands("/agent:coder:gpt-5.5-high fix tests");

        let command = commands[0].parsed.as_ref().unwrap();
        assert_eq!(command.agent.as_deref(), Some("coder"));
        assert_eq!(command.model.as_deref(), Some("gpt-5.5-high"));
    }

    #[test]
    fn ignores_words_that_only_start_with_agent_prefix() {
        let commands =
            extract_commands("/agentic no\n/agents no\n/agent ping\n/agent:reviewer review");

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].raw, "/agent ping");
        assert_eq!(commands[1].raw, "/agent:reviewer review");
    }
}
