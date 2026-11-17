//! What a program does with a project on disk, through the same code as
//! the CLI: vetting, installing descriptors, and checking. A [Project]
//! names its directories, and the [DescriptorStore] is the caller's, so
//! nothing here reads the process's working directory or its signal
//! handlers, or changes its environment. Of the environment, it reads only
//! what `slcr` itself reads: the `PATH` a check falls back to, the
//! variables that locate git's global excludes, and, while fetching, the
//! proxy variables.

use std::future::Future;
use std::path::Path;

use miette::Result;

use crate::check::flow::{self, Observer};
use crate::check::json::Document;
use crate::check::{Options, Report, ValidatorStatus};
use crate::cli::init::{Initialized, Request, initialize};
use crate::fs::FileSystem;
use crate::project::vet::{VettedProject, vet_project};
use crate::spec::text::{SpecName, Title};
use crate::tools::add::{Added, add};
use crate::tools::install::{Installed, install};
use crate::tools::manifest::Project;
use crate::tools::remove::{Removed, remove};
use crate::tools::source::Fetcher;
use crate::tools::source::SourceArg;
use crate::tools::store::DescriptorStore;
use crate::tools::update::{Updated, update};
use crate::tools::{Environment, ToolKey};
use crate::vet::{Outcome, ProjectOutcome};

impl Project {
    /// Create a specification at `file`, relative to `slcr_dir` or
    /// absolute, named `name` and titled `title`, or else `name`, as `slcr
    /// init --slcr-dir` does, and open its project, whose project root is
    /// `project_root`. Both directories must be absolute and exist.
    ///
    /// The file must be inside `slcr_dir`. If `slcr_dir` holds no
    /// `SLCR.toml`, it creates one there that lists the file; if it holds
    /// one, it joins that project, and [Initialized::warning] says if its
    /// `specs` don't match the file. It never overwrites a file or creates a
    /// directory, and creates both files or neither.
    pub fn create(
        slcr_dir: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        file: impl AsRef<Path>,
        name: SpecName,
        title: Option<Title>,
    ) -> Result<(Project, Initialized)> {
        let (slcr_dir, project_root) = (slcr_dir.as_ref(), project_root.as_ref());
        // Both directories are checked before anything is written.
        let (resolved, _) = Self::resolve_absolute(slcr_dir, project_root)?;
        let request = Request {
            file: file.as_ref(),
            name: Some(name),
            title,
            slcr_dir: Some(slcr_dir),
            inside: true,
        };
        let initialized = initialize(&FileSystem::detached(), &resolved, request)?;
        let project = Self::open(slcr_dir, project_root)?;
        Ok((project, initialized))
    }

    /// The environment the commands run in, for this project: files named
    /// relative to its SLCR directory, as `slcr` names them run there.
    fn environment<'a>(
        &'a self,
        fs: &'a FileSystem,
        store: &'a DescriptorStore,
        fetcher: &'a Fetcher,
    ) -> Environment<'a> {
        Environment {
            fs,
            store,
            fetcher,
            cwd: self.slcr_dir(),
            project: self.clone(),
        }
    }

    /// Vet the project, with the descriptors its tools pin as `store`
    /// holds them, as `slcr vet --project-root ROOT` does run in its SLCR
    /// directory: the manifest, every specification its `specs` match,
    /// every validator file, and every pinned descriptor. Nothing is
    /// fetched or run.
    ///
    /// It returns what vetting the project as a whole found, and each
    /// specification file's outcome, in the order of their paths;
    /// [crate::vet::json] writes them as `slcr vet --format json` does.
    pub fn vet(&self, store: &DescriptorStore) -> (ProjectOutcome, Vec<Outcome>) {
        let fs = FileSystem::detached();
        let VettedProject {
            project, outcomes, ..
        } = vet_project(&fs, store, self, self.slcr_dir());
        (project, outcomes)
    }

    /// Install every descriptor the project's tools pin that `store`
    /// lacks, fetched with `fetcher`, as `slcr tools install --slcr-dir`
    /// does: never re-pinning one, never writing the manifest. One tool
    /// failing doesn't stop the rest; [Installed] displays each tool's
    /// status line, as `slcr tools install` writes them.
    ///
    /// It fails only if the manifest can't be read. This, declaring a tool,
    /// and updating one are all that fetch descriptors.
    pub async fn install(&self, store: &DescriptorStore, fetcher: &Fetcher) -> Result<Installed> {
        let fs = FileSystem::detached();
        install(&self.environment(&fs, store, fetcher)).await
    }

    /// Declare a tool from `source` in the project's manifest, as `slcr
    /// tools add SOURCE --slcr-dir` does: fetch its descriptor with
    /// `fetcher`, check it, pin it, install it in `store`, and only then
    /// write the manifest, under `name`, or else the descriptor's own name.
    ///
    /// `source` is an `https://` URL, or the path of a file inside the SLCR
    /// directory, relative to it or absolute, which the manifest records
    /// relative to it. If it fails, nothing changes.
    pub async fn add_tool(
        &self,
        store: &DescriptorStore,
        fetcher: &Fetcher,
        source: &str,
        name: Option<ToolKey>,
    ) -> Result<Added> {
        let source: SourceArg = source.parse()?;
        let fs = FileSystem::detached();
        add(&self.environment(&fs, store, fetcher), source, name).await
    }

    /// Re-fetch the descriptor of the tool declared under `name` from its
    /// source with `fetcher`, check it, install it in `store`, and re-pin
    /// its entry, as `slcr tools update NAME --slcr-dir` does. If it fails,
    /// nothing changes.
    pub async fn update_tool(
        &self,
        store: &DescriptorStore,
        fetcher: &Fetcher,
        name: ToolKey,
    ) -> Result<Updated> {
        let fs = FileSystem::detached();
        update(&self.environment(&fs, store, fetcher), name).await
    }

    /// Remove the tool declared under `name` from the project's manifest,
    /// as `slcr tools remove NAME --slcr-dir` does. Its descriptor stays in
    /// any store that holds it, and nothing is fetched.
    pub fn remove_tool(&self, name: ToolKey) -> Result<Removed> {
        remove(&FileSystem::detached(), self, name)
    }

    /// Check the project, with the descriptors `store` holds, as `slcr
    /// check --offline` does: vet it, and, unless vetting fails, run every
    /// validator against its project root and evaluate each specification.
    /// It never installs a descriptor: one `store` lacks fails vetting.
    ///
    /// It runs on the caller's tokio runtime, which must have its I/O and
    /// time drivers enabled, and starts none of its own. Dropping it
    /// before it completes stops it too: on Unix, every validator's process
    /// group still alive is killed there and then, whether or not the
    /// runtime runs again; on Windows, only each validator's own process is
    /// killed, and only once the runtime next runs its task. `on_status` is
    /// given each validator's status, the line `slcr check` writes for
    /// it, as soon as `slcr check` would write it, in the same order.
    /// When `cancel` completes, the check stops as `slcr check` does when
    /// interrupted: every validator still running is stopped, with every
    /// process it started, no other starts, and the report says it was
    /// interrupted. Nothing listens for Ctrl-C.
    ///
    /// It returns the report `slcr check --format json` writes.
    pub async fn check(
        &self,
        store: &DescriptorStore,
        options: &Options,
        on_status: impl FnMut(&ValidatorStatus) + Send,
        cancel: impl Future<Output = ()> + Send,
    ) -> Report {
        let fs = FileSystem::detached();
        // Never used: a check that doesn't install never fetches.
        let fetcher = Fetcher::new();
        let env = self.environment(&fs, store, &fetcher);
        let options = flow::Options {
            install: false,
            jobs: options.jobs,
            deny_warnings: options.deny_warnings,
            path: options.path.clone(),
        };
        let mut reporter = Reporter {
            on_status,
            report: None,
        };
        // The verdict is the report's `passed`, and the reporter never
        // fails.
        let _ = flow::check(&env, &options, cancel, &mut reporter).await;
        reporter
            .report
            .expect("a check reports what it did, however far it got")
    }
}

/// The library's observer of a check: it passes each status on, and keeps
/// the report.
struct Reporter<F> {
    on_status: F,
    report: Option<Report>,
}

impl<F: FnMut(&ValidatorStatus)> Observer for Reporter<F> {
    fn status(&mut self, status: &ValidatorStatus) -> Result<()> {
        (self.on_status)(status);
        Ok(())
    }

    fn reported(&mut self, document: &Document<'_>) -> Result<()> {
        let json = serde_json::to_value(document).expect("what `check` did serializes to JSON");
        let report =
            serde_json::from_value(json).expect("`check::Report` reads what `check` writes");
        self.report = Some(report);
        Ok(())
    }
}
