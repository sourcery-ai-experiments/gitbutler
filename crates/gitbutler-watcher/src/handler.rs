use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use gitbutler_branch_actions::VirtualBranches;
use gitbutler_command_context::ProjectRepository;
use gitbutler_error::error::Marker;
use gitbutler_oplog::{
    entry::{OperationKind, SnapshotDetails},
    OplogExt,
};
use gitbutler_project as projects;
use gitbutler_project::ProjectId;
use gitbutler_reference::{LocalRefname, Refname};
use gitbutler_sync::cloud::sync_with_gitbutler;
use gitbutler_user as users;
use tracing::instrument;

use super::{events, Change};

/// A type that contains enough state to make decisions based on changes in the filesystem, which themselves
/// may trigger [Changes](Change)
// NOTE: This is `Clone` as each incoming event is spawned onto a thread for processing.
#[derive(Clone)]
pub struct Handler {
    // The following fields our currently required state as we are running in the background
    // and access it as filesystem events are processed. It's still to be decided how granular it
    // should be, and I can imagine having a top-level `app` handle that keeps the application state of
    // the tauri app, assuming that such application would not be `Send + Sync` everywhere and thus would
    // need extra protection.
    projects: projects::Controller,
    users: users::Controller,
    vbranch_controller: gitbutler_branch_actions::VirtualBranchActions,

    /// A function to send events - decoupled from app-handle for testing purposes.
    #[allow(clippy::type_complexity)]
    send_event: Arc<dyn Fn(Change) -> Result<()> + Send + Sync + 'static>,
}

impl Handler {
    /// A constructor whose primary use is the test-suite.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        projects: projects::Controller,
        users: users::Controller,
        vbranch_controller: gitbutler_branch_actions::VirtualBranchActions,
        send_event: impl Fn(Change) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Handler {
            projects,
            users,
            vbranch_controller,
            send_event: Arc::new(send_event),
        }
    }

    /// Handle the events that come in from the filesystem, or the public API.
    #[instrument(skip(self), fields(event = %event), err(Debug))]
    pub(super) async fn handle(&self, event: events::InternalEvent) -> Result<()> {
        match event {
            events::InternalEvent::ProjectFilesChange(project_id, path) => {
                self.recalculate_everything(path, project_id).await
            }

            events::InternalEvent::GitFilesChange(project_id, paths) => self
                .git_files_change(paths, project_id)
                .await
                .context("failed to handle git file change event"),

            events::InternalEvent::GitButlerOplogChange(project_id) => self
                .gitbutler_oplog_change(project_id)
                .await
                .context("failed to handle gitbutler oplog change event"),

            // This is only produced at the end of mutating Tauri commands to trigger a fresh state being served to the UI.
            events::InternalEvent::CalculateVirtualBranches(project_id) => self
                .calculate_virtual_branches(project_id)
                .await
                .context("failed to handle virtual branch event"),
        }
    }
}

impl Handler {
    fn emit_app_event(&self, event: Change) -> Result<()> {
        (self.send_event)(event).context("failed to send event")
    }

    #[instrument(skip(self, project_id))]
    async fn calculate_virtual_branches(&self, project_id: ProjectId) -> Result<()> {
        let project = self
            .projects
            .get(project_id)
            .context("failed to get project")?;
        match self
            .vbranch_controller
            .list_virtual_branches(&project)
            .await
        {
            Ok((branches, skipped_files)) => self.emit_app_event(Change::VirtualBranches {
                project_id: project.id,
                virtual_branches: VirtualBranches {
                    branches,
                    skipped_files,
                },
            }),
            Err(err)
                if matches!(
                    err.downcast_ref::<Marker>(),
                    Some(Marker::VerificationFailure)
                ) =>
            {
                Ok(())
            }
            Err(err) => Err(err.context("failed to list virtual branches")),
        }
    }

    #[instrument(skip(self, paths, project_id), fields(paths = paths.len()))]
    async fn recalculate_everything(
        &self,
        paths: Vec<PathBuf>,
        project_id: ProjectId,
    ) -> Result<()> {
        self.maybe_create_snapshot(project_id).ok();
        self.calculate_virtual_branches(project_id).await?;
        Ok(())
    }

    fn maybe_create_snapshot(&self, project_id: ProjectId) -> anyhow::Result<()> {
        let project = self
            .projects
            .get(project_id)
            .context("failed to get project")?;
        if project
            .should_auto_snapshot(std::time::Duration::from_secs(300))
            .unwrap_or_default()
        {
            let mut guard = project.exclusive_worktree_access();
            project.create_snapshot(
                SnapshotDetails::new(OperationKind::FileChanges),
                guard.write_permission(),
            )?;
        }
        Ok(())
    }

    pub async fn git_files_change(&self, paths: Vec<PathBuf>, project_id: ProjectId) -> Result<()> {
        let project = self
            .projects
            .get(project_id)
            .context("failed to get project")?;
        let open_projects_repository = || {
            ProjectRepository::open(&project.clone())
                .context("failed to open project repository for project")
        };

        for path in paths {
            let Some(file_name) = path.to_str() else {
                continue;
            };
            match file_name {
                "FETCH_HEAD" => {
                    self.emit_app_event(Change::GitFetch(project_id))?;
                }
                "logs/HEAD" => {
                    self.emit_app_event(Change::GitActivity(project.id))?;
                }
                "HEAD" => {
                    let project_repository = open_projects_repository()?;
                    let head_ref = project_repository
                        .repo()
                        .head()
                        .context("failed to get head")?;
                    let head_ref_name = head_ref.name().context("failed to get head name")?;
                    if head_ref_name != "refs/heads/gitbutler/integration" {
                        let mut integration_reference = project_repository.repo().find_reference(
                            &Refname::from(LocalRefname::new("gitbutler/integration", None))
                                .to_string(),
                        )?;
                        integration_reference.delete()?;
                    }
                    if let Some(head) = head_ref.name() {
                        self.emit_app_event(Change::GitHead {
                            project_id,
                            head: head.to_string(),
                        })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Invoked whenever there's a new oplog entry.
    /// If synchronizing with GitButler's servers is enabled it will push Oplog refs
    async fn gitbutler_oplog_change(&self, project_id: ProjectId) -> Result<()> {
        let project = self
            .projects
            .get(project_id)
            .context("failed to get project")?;

        if project.is_sync_enabled() && project.has_code_url() {
            if let Some(user) = self.users.get_user()? {
                let repository = ProjectRepository::open(&project)
                    .context("failed to open project repository for project")?;
                return sync_with_gitbutler(&repository, &user, &self.projects).await;
            }
        }
        Ok(())
    }
}
