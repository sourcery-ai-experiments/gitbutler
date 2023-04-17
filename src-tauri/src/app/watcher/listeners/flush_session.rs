use std::sync;

use anyhow::{anyhow, Context, Result};

use crate::{
    app::{gb_repository, project_repository},
    events, projects, sessions,
};

pub struct Listener<'listener> {
    project_id: String,
    gb_repository: &'listener gb_repository::Repository,
    project_store: projects::Storage,
    sender: sync::mpsc::Sender<events::Event>,
}

impl<'listener> Listener<'listener> {
    pub fn new(
        project_id: String,
        project_store: projects::Storage,
        gb_repository: &'listener gb_repository::Repository,
        sender: sync::mpsc::Sender<events::Event>,
    ) -> Self {
        Self {
            project_id,
            gb_repository,
            project_store,
            sender,
        }
    }

    pub fn register(&self, session: &sessions::Session) -> Result<()> {
        let project = self
            .project_store
            .get_project(&self.project_id)
            .context("failed to get project")?
            .ok_or_else(|| anyhow!("project not found"))?;

        let session = self
            .gb_repository
            .flush_session(&project_repository::Repository::open(&project)?, session)
            .context("failed to flush session")?;

        if let Err(e) = self.sender.send(events::Event::session(&project, &session)) {
            log::error!("failed to send session event: {}", e);
        }

        Ok(())
    }
}
