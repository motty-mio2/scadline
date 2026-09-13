use std::path::Path;

use git2::{Oid, Repository, Sort, build::RepoBuilder};

use crate::domain::{GitCommit, GitHistory, GitRevision};

pub(crate) struct GitRepository;

impl GitRepository {
    pub(crate) fn checkout_revision(
        revision: &GitRevision,
        destination: &Path,
    ) -> Result<(), String> {
        let repository = RepoBuilder::new()
            .clone(revision.root.to_string_lossy().as_ref(), destination)
            .map_err(|error| format!("Gitスナップショットを準備できません: {error}"))?;
        let commit_id = Oid::from_str(&revision.commit.hash)
            .map_err(|error| format!("Git commit IDが不正です: {error}"))?;
        let commit = repository
            .find_commit(commit_id)
            .map_err(|error| format!("Git commitを読み込めません: {error}"))?;
        repository
            .checkout_tree(commit.as_object(), None)
            .map_err(|error| format!("Git treeを展開できません: {error}"))?;
        repository
            .set_head_detached(commit_id)
            .map_err(|error| format!("Git HEADを設定できません: {error}"))
    }

    pub(crate) fn discover(source: &Path) -> Option<GitHistory> {
        let source = source.canonicalize().ok()?;
        let repository = Repository::discover(&source).ok()?;
        let root = repository.workdir()?.canonicalize().ok()?;
        let relative_path = source.strip_prefix(&root).ok()?.to_owned();

        let mut branches: Vec<String> = repository
            .branches(None)
            .ok()?
            .flatten()
            .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
            .filter(|branch| !branch.ends_with("/HEAD"))
            .collect();
        branches.sort();
        branches.dedup();
        if branches.is_empty() {
            return None;
        }

        let current_branch = repository
            .head()
            .ok()
            .and_then(|head| head.shorthand().ok().map(str::to_owned));
        let selected_branch = current_branch
            .as_ref()
            .and_then(|current| branches.iter().position(|branch| branch == current))
            .unwrap_or(0);

        let mut history = GitHistory {
            root,
            relative_path,
            branches,
            selected_branch,
            commits: Vec::new(),
            position: 0,
        };
        Self::load_commits(&mut history);
        Some(history)
    }

    pub(crate) fn load_commits(history: &mut GitHistory) {
        history.commits.clear();
        history.position = 0;
        let Some(branch) = history.branches.get(history.selected_branch) else {
            return;
        };
        let Ok(repository) = Repository::open(&history.root) else {
            return;
        };
        let Ok(reference) = repository.revparse_single(branch) else {
            return;
        };
        let Ok(commit) = reference.peel_to_commit() else {
            return;
        };
        let Ok(mut walk) = repository.revwalk() else {
            return;
        };
        if walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).is_err()
            || walk.push(commit.id()).is_err()
        {
            return;
        }

        history.commits = walk
            .flatten()
            .filter_map(|id| repository.find_commit(id).ok())
            .map(|commit| {
                let hash = commit.id().to_string();
                GitCommit {
                    short_hash: hash.chars().take(7).collect(),
                    hash,
                    date: format_git_date(commit.time().seconds(), commit.time().offset_minutes()),
                    subject: commit
                        .summary()
                        .ok()
                        .flatten()
                        .unwrap_or("(no message)")
                        .to_owned(),
                }
            })
            .collect();
        history.commits.reverse();
        history.position = history.commits.len();
    }
}

fn format_git_date(seconds: i64, offset_minutes: i32) -> String {
    let local_seconds = seconds.saturating_add(i64::from(offset_minutes) * 60);
    let days = local_seconds.div_euclid(86_400);
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;

    #[test]
    fn formats_git_timestamp_with_its_offset() {
        assert_eq!(format_git_date(0, 0), "1970-01-01");
        assert_eq!(format_git_date(0, -60), "1969-12-31");
    }

    #[test]
    fn discovers_history_without_a_git_executable() {
        let temporary = tempfile::tempdir().expect("create repository directory");
        let repository = Repository::init(temporary.path()).expect("initialize repository");
        let model_path = temporary.path().join("model.scad");
        std::fs::write(&model_path, "cube(10);").expect("write model");

        let mut index = repository.index().expect("open index");
        index.add_path(Path::new("model.scad")).expect("add model");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repository.find_tree(tree_id).expect("find tree");
        let signature =
            Signature::now("Scadline test", "test@scadline.invalid").expect("create signature");
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Initial model",
                &tree,
                &[],
            )
            .expect("commit model");

        let mut history = GitRepository::discover(&model_path).expect("discover history");
        assert_eq!(history.relative_path, Path::new("model.scad"));
        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].subject, "Initial model");

        history.position = 0;
        let revision = history.selected_revision().expect("select revision");
        let checkout = tempfile::tempdir().expect("create checkout parent");
        let destination = checkout.path().join("snapshot");
        GitRepository::checkout_revision(&revision, &destination).expect("checkout revision");
        assert_eq!(
            std::fs::read_to_string(destination.join("model.scad")).expect("read checkout"),
            "cube(10);"
        );
    }
}
