use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct GitCommit {
    pub(crate) hash: String,
    pub(crate) short_hash: String,
    pub(crate) date: String,
    pub(crate) subject: String,
}

pub(crate) struct GitHistory {
    pub(crate) root: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) branches: Vec<String>,
    pub(crate) selected_branch: usize,
    pub(crate) commits: Vec<GitCommit>,
    /// Commits are oldest-to-newest; commits.len() represents the working tree.
    pub(crate) position: usize,
}

impl GitHistory {
    pub(crate) fn selected_revision(&self) -> Option<GitRevision> {
        let commit = self.commits.get(self.position)?;
        Some(GitRevision {
            root: self.root.clone(),
            relative_path: self.relative_path.clone(),
            commit: commit.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct GitRevision {
    pub(crate) root: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) commit: GitCommit,
}
