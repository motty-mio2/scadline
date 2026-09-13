use std::{path::Path, sync::Arc, time::SystemTime};

use crate::{
    domain::{GitRevision, Mesh},
    infrastructure::ModelLoader,
};

pub(crate) enum RenderOutcome {
    Finished {
        mesh: Mesh,
        source_modified: Option<SystemTime>,
        cache_key: Option<String>,
        loaded_from_disk_cache: bool,
    },
    Failed(String),
}

pub(crate) struct ModelService {
    loader: Arc<ModelLoader>,
}

impl ModelService {
    pub(crate) fn new(loader: Arc<ModelLoader>) -> Self {
        Self { loader }
    }

    pub(crate) fn render(
        &self,
        source: &Path,
        revision: Option<&GitRevision>,
        cache_key: Option<String>,
    ) -> RenderOutcome {
        let source_modified = revision
            .is_none()
            .then(|| {
                source
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
            })
            .flatten();
        let (result, loaded_from_disk_cache) = if let Some(revision) = revision {
            match self.loader.render_revision(revision) {
                Ok((mesh, from_cache)) => (Ok(mesh), from_cache),
                Err(error) => (Err(error), false),
            }
        } else {
            (self.loader.render_worktree(source), false)
        };

        match result {
            Ok(mesh) => RenderOutcome::Finished {
                mesh,
                source_modified,
                cache_key,
                loaded_from_disk_cache,
            },
            Err(error) => RenderOutcome::Failed(error),
        }
    }

    pub(crate) fn prefetch(&self, revision: &GitRevision) {
        let _ = self.loader.render_revision(revision);
    }

    pub(crate) fn is_cached(&self, revision: &GitRevision) -> bool {
        self.loader.is_cached(revision)
    }
}
