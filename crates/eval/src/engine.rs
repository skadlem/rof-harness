use crate::slices::Instance;
use std::path::Path;

/// Container engine seam. Docker first; Modal/Daytona are later backends.
pub trait Engine: Send + Sync {
    fn run_instance(
        &self,
        _instance: &Instance,
        _workdir: &Path,
    ) -> std::io::Result<ContainerOutcome>;
}

#[derive(Debug, Clone)]
pub struct ContainerOutcome {
    pub reward: Option<bool>,
    pub logs: String,
}

/// Docker-first engine over std Command (no new crates). Infra errors
/// surface as `Err` so they are never scored as capability.
#[derive(Debug, Clone, Default)]
pub struct DockerEngine;

impl DockerEngine {
    pub fn new() -> Self {
        Self
    }
}

impl Engine for DockerEngine {
    fn run_instance(
        &self,
        instance: &Instance,
        workdir: &Path,
    ) -> std::io::Result<ContainerOutcome> {
        let out = std::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "-v",
                &format!("{}:/work", workdir.display()),
                &instance.image,
            ])
            .output()?;
        let mut logs = String::from_utf8_lossy(&out.stdout).into_owned();
        logs.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(ContainerOutcome { reward: None, logs })
    }
}
