use std::{
    collections::hash_map::DefaultHasher,
    fs::OpenOptions,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::SystemTime,
};
use tempfile::{Builder, NamedTempFile};

use crate::domain::{GitRevision, Mesh};

use super::{GitRepository, load_stl};

pub(crate) struct PlatformCacheLocation;

pub(crate) trait CacheLocation: Send + Sync {
    fn base_directory(&self) -> Option<PathBuf>;
}

impl CacheLocation for PlatformCacheLocation {
    fn base_directory(&self) -> Option<PathBuf> {
        get_cache_dir()
    }
}

pub(crate) struct ModelLoader {
    cache_location: Arc<dyn CacheLocation>,
    max_cache_size_bytes: u64,
}

impl ModelLoader {
    pub(crate) fn new(cache_location: Arc<dyn CacheLocation>, max_cache_size_bytes: u64) -> Self {
        if let Some(cache_root) = cache_location.base_directory() {
            thread::spawn(move || {
                let app_cache_root = cache_root.join("scadline");
                migrate_legacy_cache(&app_cache_root);
                enforce_cache_size(&app_cache_root, max_cache_size_bytes);
            });
        }
        Self {
            cache_location,
            max_cache_size_bytes,
        }
    }

    pub(crate) fn render_worktree(&self, source: &Path) -> Result<Mesh, String> {
        let output = temporary_stl()?;
        run_openscad(source, output.path())
    }

    pub(crate) fn render_revision(&self, revision: &GitRevision) -> Result<(Mesh, bool), String> {
        load_or_render_revision(
            revision,
            self.cache_location.base_directory().as_deref(),
            self.max_cache_size_bytes,
        )
    }

    pub(crate) fn is_cached(&self, revision: &GitRevision) -> bool {
        self.cache_location
            .base_directory()
            .is_some_and(|root| persistent_cache_path(&root, revision).is_file())
    }
}

#[cfg(target_os = "linux")]
fn get_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
}

#[cfg(target_os = "macos")]
fn get_cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library").join("Caches"))
}

#[cfg(target_os = "windows")]
fn get_cache_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn get_cache_dir() -> Option<PathBuf> {
    None
}

fn persistent_cache_path(cache_root: &Path, revision: &GitRevision) -> PathBuf {
    let mut repo_hasher = DefaultHasher::new();
    revision.root.hash(&mut repo_hasher);
    let repo_key = repo_hasher.finish();

    let mut path_hasher = DefaultHasher::new();
    revision.relative_path.hash(&mut path_hasher);
    let path_key = path_hasher.finish();

    cache_root
        .join("scadline")
        .join(format!("{repo_key:016x}"))
        .join(&revision.commit.hash)
        .join(format!("{path_key:016x}.stl"))
}

fn migrate_legacy_cache(cache_root: &Path) {
    fn merge_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
        if !source.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                merge_directory(&source_path, &destination_path)?;
            } else if destination_path.exists() {
                let _ = std::fs::remove_file(source_path);
            } else if std::fs::rename(&source_path, &destination_path).is_err() {
                std::fs::copy(&source_path, &destination_path)?;
                std::fs::remove_file(source_path)?;
            }
        }
        let _ = std::fs::remove_dir(source);
        Ok(())
    }

    let legacy_v2 = cache_root.join("stl-v2");
    let _ = merge_directory(&legacy_v2, cache_root);
    let legacy_v1 = cache_root.join("stl-v1");
    if legacy_v1.is_dir() {
        let _ = std::fs::remove_dir_all(legacy_v1);
    }
}

fn enforce_cache_size(cache_root: &Path, max_size_bytes: u64) {
    fn collect_files(
        directory: &Path,
        files: &mut Vec<(PathBuf, u64, SystemTime)>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                collect_files(&entry.path(), files)?;
            } else if metadata.is_file() {
                files.push((
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    if collect_files(cache_root, &mut files).is_err() {
        return;
    }
    let mut total_size = files
        .iter()
        .fold(0_u64, |total, (_, size, _)| total.saturating_add(*size));
    if total_size <= max_size_bytes {
        return;
    }

    files.sort_unstable_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in files {
        if total_size <= max_size_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total_size = total_size.saturating_sub(size);
        }
    }
}

fn refresh_cache_access_time(cache_path: &Path) {
    let _ = OpenOptions::new()
        .write(true)
        .open(cache_path)
        .and_then(|file| file.set_modified(SystemTime::now()));
}

fn temporary_stl() -> Result<NamedTempFile, String> {
    Builder::new()
        .prefix("scadline-")
        .suffix(".stl")
        .tempfile()
        .map_err(|error| format!("一時STLファイルを作成できません: {error}"))
}

fn run_openscad(source: &Path, output_path: &Path) -> Result<Mesh, String> {
    let output = Command::new("openscad")
        .arg("--export-format")
        .arg("binstl")
        .arg("-o")
        .arg(output_path)
        .arg(source)
        .output()
        .map_err(|error| {
            format!(
                "OpenSCADを起動できません: {error}\nOpenSCADがインストールされているか確認してください。"
            )
        })?;
    if output.status.success() {
        load_stl(output_path)
    } else {
        Err(format!(
            "OpenSCADエラー:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn load_or_render_revision(
    revision: &GitRevision,
    cache_root: Option<&Path>,
    max_cache_size_bytes: u64,
) -> Result<(Mesh, bool), String> {
    let cache_path = cache_root.map(|root| persistent_cache_path(root, revision));
    if let Some(cache_path) = &cache_path
        && cache_path.is_file()
        && let Ok(mesh) = load_stl(cache_path)
    {
        refresh_cache_access_time(cache_path);
        return Ok((mesh, true));
    }

    let snapshot_dir = tempfile::tempdir()
        .map_err(|error| format!("一時ディレクトリを作成できません: {error}"))?;
    let snapshot_path = snapshot_dir.path().join("repository");
    let output = temporary_stl()?;

    let result = (|| -> Result<Mesh, String> {
        GitRepository::checkout_revision(revision, &snapshot_path)?;

        let mesh = run_openscad(&snapshot_path.join(&revision.relative_path), output.path())?;
        if let Some(cache_path) = &cache_path
            && let Some(parent) = cache_path.parent()
            && std::fs::create_dir_all(parent).is_ok()
            && let Ok(temporary_cache) = NamedTempFile::new_in(parent)
            && std::fs::copy(output.path(), temporary_cache.path()).is_ok()
        {
            let _ = std::fs::remove_file(cache_path);
            let _ = temporary_cache.persist(cache_path);
            if let Some(cache_root) = cache_root {
                enforce_cache_size(&cache_root.join("scadline"), max_cache_size_bytes);
            }
        }
        Ok(mesh)
    })();

    result.map(|mesh| (mesh, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn set_modified(path: &Path, time: SystemTime) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open cache fixture")
            .set_modified(time)
            .expect("set cache fixture time");
    }

    #[test]
    fn cache_pruning_keeps_total_under_limit() {
        let temporary = tempfile::tempdir().expect("create cache fixture");
        let directory = temporary.path();
        std::fs::create_dir_all(directory.join("repo/commit")).expect("create cache fixture");
        for index in 0..3 {
            std::fs::write(
                directory.join(format!("repo/commit/{index}.stl")),
                [index as u8; 10],
            )
            .expect("write cache fixture");
        }

        enforce_cache_size(directory, 15);

        let remaining_size: u64 = std::fs::read_dir(directory.join("repo/commit"))
            .expect("read cache fixture")
            .map(|entry| {
                entry
                    .expect("read entry")
                    .metadata()
                    .expect("metadata")
                    .len()
            })
            .sum();
        assert!(remaining_size <= 15);
    }

    #[test]
    fn cache_access_refresh_preserves_a_recently_used_entry() {
        let temporary = tempfile::tempdir().expect("create cache fixture");
        let directory = temporary.path();
        let accessed = directory.join("accessed.stl");
        let evicted = directory.join("evicted.stl");
        let retained = directory.join("retained.stl");
        for path in [&accessed, &evicted, &retained] {
            std::fs::write(path, [0_u8; 10]).expect("write cache fixture");
        }

        let origin = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        set_modified(&accessed, origin);
        set_modified(&evicted, origin + Duration::from_secs(1));
        set_modified(&retained, origin + Duration::from_secs(2));
        refresh_cache_access_time(&accessed);

        enforce_cache_size(directory, 20);

        assert!(accessed.exists());
        assert!(!evicted.exists());
        assert!(retained.exists());
    }

    #[test]
    fn cache_entry_larger_than_the_limit_is_not_retained() {
        let temporary = tempfile::tempdir().expect("create cache fixture");
        let entry = temporary.path().join("oversized.stl");
        std::fs::write(&entry, [0_u8; 21]).expect("write cache fixture");

        enforce_cache_size(temporary.path(), 20);

        assert!(!entry.exists());
    }
}
