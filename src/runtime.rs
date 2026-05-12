use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePlan {
    Devcontainer,
    DefaultImage(String),
}

impl RuntimePlan {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Devcontainer => "devcontainer",
            Self::DefaultImage(_) => "default-image",
        }
    }
}

pub fn detect_repo_runtime(
    repo_path: impl AsRef<Path>,
    default_image: impl Into<String>,
) -> RuntimePlan {
    let devcontainer = repo_path.as_ref().join(".devcontainer/devcontainer.json");
    if devcontainer.exists() {
        RuntimePlan::Devcontainer
    } else {
        RuntimePlan::DefaultImage(default_image.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_devcontainer() {
        let temp = tempfile::tempdir().unwrap();
        let devcontainer_dir = temp.path().join(".devcontainer");
        fs::create_dir(&devcontainer_dir).unwrap();
        fs::write(devcontainer_dir.join("devcontainer.json"), "{}").unwrap();

        assert_eq!(
            detect_repo_runtime(temp.path(), "fallback").kind(),
            "devcontainer"
        );
    }

    #[test]
    fn falls_back_to_default_image() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            detect_repo_runtime(temp.path(), "ghcr.io/example/runtime:latest"),
            RuntimePlan::DefaultImage("ghcr.io/example/runtime:latest".to_string())
        );
    }
}
