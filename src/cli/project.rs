use std::path::{Path, PathBuf};

use clap::Args;

use crate::tools::manifest::{Project, ProjectError};

/// `--slcr-dir` and `--project-root`: where the project of `slcr vet` and
/// `slcr check` is.
#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectArgs {
    /// The directory that holds SLCR.toml, the specifications, and the
    /// validator files. Only it is searched for SLCR.toml, never its
    /// parents [default: the nearest directory, from the working directory
    /// up, that holds SLCR.toml].
    #[arg(long, value_name = "DIR")]
    slcr_dir: Option<PathBuf>,

    /// The directory the validators check. A validator file checks the
    /// directory at its own path, relative to the directory of SLCR.toml,
    /// under it [default: the directory of SLCR.toml].
    #[arg(long, value_name = "DIR")]
    project_root: Option<PathBuf>,
}

impl ProjectArgs {
    /// The project these name, from the working directory `cwd`.
    pub(crate) fn locate(&self, cwd: &Path) -> Result<Project, ProjectError> {
        Project::locate(self.slcr_dir.as_deref(), self.project_root.as_deref(), cwd)
    }
}

/// `--slcr-dir`: where the manifest the `slcr tools` commands work on is.
#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub struct SlcrDirArg {
    /// The directory that holds SLCR.toml. Only it is searched for
    /// SLCR.toml, never its parents, and path sources are relative to it
    /// [default: the nearest directory, from the working directory up,
    /// that holds SLCR.toml].
    #[arg(long, value_name = "DIR", global = true)]
    slcr_dir: Option<PathBuf>,
}

impl SlcrDirArg {
    /// The project this names, from the working directory `cwd`.
    pub(crate) fn locate(&self, cwd: &Path) -> Result<Project, ProjectError> {
        Project::locate(self.slcr_dir.as_deref(), None, cwd)
    }
}
