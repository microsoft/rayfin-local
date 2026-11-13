//! Project loading shared with the CLI.

pub mod serve;

use std::path::PathBuf;

use rl_node::NodeRuntime;
use rl_project::{LoadOptions, Project, ProjectError};
use serde_json::json;

use crate::events::WarningCode;
use crate::ui;

#[derive(Clone, Debug, Default)]
pub struct Location {
    pub dir: PathBuf,
    pub state_dir: Option<PathBuf>,
}

impl Location {
    pub fn at(dir: &std::path::Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            state_dir: None,
        }
    }

    pub fn discover(&self) -> Result<Project, ProjectError> {
        Project::discover_with(
            &self.dir,
            &LoadOptions {
                state_dir: self.state_dir.clone(),
                ..LoadOptions::default()
            },
        )
    }
}

pub fn load_project(at: &Location) -> anyhow::Result<Project> {
    let project = at.discover()?;
    check_project(&project)?;
    Ok(project)
}

pub fn check_project(project: &Project) -> anyhow::Result<()> {
    for warning in project.warnings() {
        ui::warn(warning);
    }
    project.check_installed()?;
    if let Some(warning) = project.sdk_version_warning() {
        ui::warning(
            WarningCode::SdkUnsupported,
            warning,
            json!({
                "version": project.sdk_version(),
                "supported": rl_project::supported_sdk_range(),
            }),
        );
    }
    ui::step(format_args!(
        "Project {} ({})",
        project.display_name(),
        project.root().display()
    ));
    Ok(())
}

pub async fn find_node() -> anyhow::Result<NodeRuntime> {
    let node = NodeRuntime::discover().await?;
    tracing::debug!("Node.js {} at {}", node.version, node.path.display());
    Ok(node)
}
