use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs::File,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::{egui, egui_glow, glow};
use egui::mutex::Mutex;
use glow::HasContext as _;
use serde::{Deserialize, Serialize};

const DEFAULT_CACHE_SIZE_MB: u64 = 256;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct AppConfig {
    cache: CacheConfig,
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
struct CacheConfig {
    max_size_mb: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_mb: DEFAULT_CACHE_SIZE_MB,
        }
    }
}

fn config_file_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("scadline").join("config.toml"))
}

fn load_or_create_config() -> AppConfig {
    let Some(path) = config_file_path() else {
        return AppConfig::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_else(|error| {
            eprintln!("{} の解析に失敗しました: {error}", path.display());
            AppConfig::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = AppConfig::default();
            if let Some(parent) = path.parent()
                && std::fs::create_dir_all(parent).is_ok()
                && let Ok(contents) = toml::to_string_pretty(&config)
            {
                let _ = std::fs::write(path, contents);
            }
            config
        }
        Err(error) => {
            eprintln!("{} を読み込めません: {error}", path.display());
            AppConfig::default()
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Scadline")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([720.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        ..Default::default()
    };

    eframe::run_native(
        "Scadline",
        options,
        Box::new(|cc| Ok(Box::new(ScadlineApp::new(cc)))),
    )
}

#[derive(Clone, Copy, Default)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }

    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }

    fn mul(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }

    fn dot(self, rhs: Self) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    fn cross(self, rhs: Self) -> Self {
        Self::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    fn normalized(self) -> Self {
        let length = self.dot(self).sqrt();
        if length > 0.000_001 {
            self.mul(1.0 / length)
        } else {
            self
        }
    }
}

fn point_in_convex_polygon(point: Pos2, polygon: &[Pos2]) -> bool {
    let mut sign = 0.0_f32;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
        if cross.abs() > 0.001 {
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
    }
    true
}

#[derive(Clone)]
struct Mesh {
    vertices: Vec<Vec3>,
    triangles: Vec<[usize; 3]>,
    center: Vec3,
    radius: f32,
}

impl Mesh {
    fn from_stl(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("STLを開けません: {e}"))?;
        let indexed = stl_io::read_stl(&mut file).map_err(|e| format!("STLの解析に失敗: {e}"))?;
        if indexed.vertices.is_empty() || indexed.faces.is_empty() {
            return Err("OpenSCADの出力に面がありません".to_owned());
        }

        let vertices: Vec<Vec3> = indexed
            .vertices
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let triangles = indexed.faces.iter().map(|face| face.vertices).collect();

        let mut min = vertices[0];
        let mut max = vertices[0];
        for vertex in &vertices[1..] {
            min.x = min.x.min(vertex.x);
            min.y = min.y.min(vertex.y);
            min.z = min.z.min(vertex.z);
            max.x = max.x.max(vertex.x);
            max.y = max.y.max(vertex.y);
            max.z = max.z.max(vertex.z);
        }
        let center = min.add(max).mul(0.5);
        let radius = vertices
            .iter()
            .map(|v| v.sub(center).dot(v.sub(center)).sqrt())
            .fold(0.0_f32, f32::max)
            .max(0.001);

        Ok(Self {
            vertices,
            triangles,
            center,
            radius,
        })
    }
}

enum RenderMessage {
    Finished {
        mesh: Mesh,
        source_modified: Option<SystemTime>,
        cache_key: Option<String>,
        loaded_from_disk_cache: bool,
    },
    Failed(String),
}

#[derive(Clone)]
struct GitCommit {
    hash: String,
    short_hash: String,
    date: String,
    subject: String,
}

struct GitHistory {
    root: PathBuf,
    relative_path: PathBuf,
    branches: Vec<String>,
    selected_branch: usize,
    commits: Vec<GitCommit>,
    /// Commits are oldest-to-newest; commits.len() represents the working tree.
    position: usize,
}

impl GitHistory {
    fn discover(source: &Path) -> Option<Self> {
        let source = source.canonicalize().ok()?;
        let parent = source.parent()?;
        let root_output = Command::new("git")
            .arg("-C")
            .arg(parent)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()?;
        if !root_output.status.success() {
            return None;
        }
        let root = PathBuf::from(String::from_utf8(root_output.stdout).ok()?.trim());
        let relative_path = source.strip_prefix(&root).ok()?.to_owned();

        let branch_output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args([
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
                "refs/remotes",
            ])
            .output()
            .ok()?;
        let mut branches: Vec<String> = String::from_utf8_lossy(&branch_output.stdout)
            .lines()
            .filter(|branch| !branch.ends_with("/HEAD"))
            .map(str::to_owned)
            .collect();
        branches.sort();
        branches.dedup();
        if branches.is_empty() {
            return None;
        }

        let current_branch = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["branch", "--show-current"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        let selected_branch = current_branch
            .as_ref()
            .and_then(|current| branches.iter().position(|branch| branch == current))
            .unwrap_or(0);

        let mut history = Self {
            root,
            relative_path,
            branches,
            selected_branch,
            commits: Vec::new(),
            position: 0,
        };
        history.load_commits();
        Some(history)
    }

    fn load_commits(&mut self) {
        self.commits.clear();
        self.position = 0;
        let Some(branch) = self.branches.get(self.selected_branch) else {
            return;
        };
        let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("log")
            .arg(branch)
            .arg("--format=%H%x1f%h%x1f%cs%x1f%s")
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        self.commits = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(4, '\x1f');
                Some(GitCommit {
                    hash: fields.next()?.to_owned(),
                    short_hash: fields.next()?.to_owned(),
                    date: fields.next()?.to_owned(),
                    subject: fields.next()?.to_owned(),
                })
            })
            .collect();
        self.commits.reverse();
        self.position = self.commits.len();
    }

    fn selected_revision(&self) -> Option<GitRevision> {
        let commit = self.commits.get(self.position)?;
        Some(GitRevision {
            root: self.root.clone(),
            relative_path: self.relative_path.clone(),
            commit: commit.clone(),
        })
    }
}

#[derive(Clone)]
struct GitRevision {
    root: PathBuf,
    relative_path: PathBuf,
    commit: GitCommit,
}

trait CacheLocation: Send + Sync {
    fn base_directory(&self) -> Option<PathBuf>;
}

struct PlatformCacheLocation;

impl CacheLocation for PlatformCacheLocation {
    fn base_directory(&self) -> Option<PathBuf> {
        get_cache_dir()
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

fn unique_render_token() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn run_openscad(source: &Path, output_path: &Path) -> Result<Mesh, String> {
    let output = Command::new("openscad")
        .arg("--export-format")
        .arg("binstl")
        .arg("-o")
        .arg(output_path)
        .arg(source)
        .output()
        .map_err(|e| {
            format!(
                "OpenSCADを起動できません: {e}\nOpenSCADがインストールされているか確認してください。"
            )
        })?;
    if output.status.success() {
        Mesh::from_stl(output_path)
    } else {
        Err(format!(
            "OpenSCADエラー:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn render_worktree(source: &Path) -> Result<Mesh, String> {
    let output_path = std::env::temp_dir().join(format!("scadline-{}.stl", unique_render_token()));
    let result = run_openscad(source, &output_path);
    let _ = std::fs::remove_file(output_path);
    result
}

fn load_or_render_revision(
    revision: &GitRevision,
    cache_root: Option<&Path>,
    max_cache_size_bytes: u64,
) -> Result<(Mesh, bool), String> {
    let cache_path = cache_root.map(|root| persistent_cache_path(root, revision));
    if let Some(cache_path) = &cache_path
        && cache_path.is_file()
        && let Ok(mesh) = Mesh::from_stl(cache_path)
    {
        return Ok((mesh, true));
    }

    let token = unique_render_token();
    let snapshot_dir = std::env::temp_dir().join(format!("scadline-snapshot-{token}"));
    let archive_path = std::env::temp_dir().join(format!("scadline-snapshot-{token}.tar"));
    let output_path = std::env::temp_dir().join(format!("scadline-{token}.stl"));

    let result = (|| -> Result<Mesh, String> {
        std::fs::create_dir_all(&snapshot_dir)
            .map_err(|e| format!("一時ディレクトリを作成できません: {e}"))?;
        let archive_output = Command::new("git")
            .arg("-C")
            .arg(&revision.root)
            .args(["archive", "--format=tar", "-o"])
            .arg(&archive_path)
            .arg(&revision.commit.hash)
            .output()
            .map_err(|e| format!("Gitスナップショットを作成できません: {e}"))?;
        if !archive_output.status.success() {
            return Err(format!(
                "git archive エラー: {}",
                String::from_utf8_lossy(&archive_output.stderr).trim()
            ));
        }
        let extract_output = Command::new("tar")
            .arg("-xf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&snapshot_dir)
            .output()
            .map_err(|e| format!("Gitスナップショットを展開できません: {e}"))?;
        if !extract_output.status.success() {
            return Err(format!(
                "tar エラー: {}",
                String::from_utf8_lossy(&extract_output.stderr).trim()
            ));
        }

        let mesh = run_openscad(&snapshot_dir.join(&revision.relative_path), &output_path)?;
        if let Some(cache_path) = &cache_path
            && let Some(parent) = cache_path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let temporary_cache = cache_path.with_extension(format!("stl.{token}.tmp"));
            if std::fs::copy(&output_path, &temporary_cache).is_ok() {
                let _ = std::fs::rename(&temporary_cache, cache_path);
                let _ = std::fs::remove_file(temporary_cache);
                if let Some(cache_root) = cache_root {
                    enforce_cache_size(&cache_root.join("scadline"), max_cache_size_bytes);
                }
            }
        }
        Ok(mesh)
    })();

    let _ = std::fs::remove_file(output_path);
    let _ = std::fs::remove_file(archive_path);
    let _ = std::fs::remove_dir_all(snapshot_dir);
    result.map(|mesh| (mesh, false))
}

struct ScadlineApp {
    source_path: Option<PathBuf>,
    mesh: Option<Mesh>,
    result_rx: Option<Receiver<RenderMessage>>,
    is_rendering: bool,
    render_pending: bool,
    timeline_changed_at: Option<Instant>,
    status: String,
    last_source_modified: Option<SystemTime>,
    last_watch_check: Instant,
    auto_reload: bool,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    pan: Vec2,
    show_edges: bool,
    show_axes: bool,
    background: Color32,
    renderer: Arc<Mutex<SceneRenderer>>,
    git_history: Option<GitHistory>,
    mesh_cache: HashMap<String, Mesh>,
    cache_location: Arc<dyn CacheLocation>,
    prefetch_rx: Option<Receiver<()>>,
    prefetch_attempted: HashSet<String>,
    idle_since: Instant,
    max_cache_size_bytes: u64,
}

impl ScadlineApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::new_with_cache_location(cc, Arc::new(PlatformCacheLocation))
    }

    fn new_with_cache_location(
        cc: &eframe::CreationContext<'_>,
        cache_location: Arc<dyn CacheLocation>,
    ) -> Self {
        let config = load_or_create_config();
        let max_cache_size_bytes = config.cache.max_size_mb.saturating_mul(1024 * 1024);
        if let Some(cache_root) = cache_location.base_directory() {
            thread::spawn(move || {
                let app_cache_root = cache_root.join("scadline");
                migrate_legacy_cache(&app_cache_root);
                enforce_cache_size(&app_cache_root, max_cache_size_bytes);
            });
        }
        configure_japanese_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let gl = cc.gl.as_ref().expect("Glow renderer is required");
        let mut app = Self {
            source_path: None,
            mesh: None,
            result_rx: None,
            is_rendering: false,
            render_pending: false,
            timeline_changed_at: None,
            status: "OpenSCADファイルを開いてください".to_owned(),
            last_source_modified: None,
            last_watch_check: Instant::now(),
            auto_reload: true,
            yaw: -0.65,
            pitch: 0.55,
            zoom: 1.0,
            pan: Vec2::ZERO,
            show_edges: true,
            show_axes: true,
            background: Color32::from_rgb(24, 27, 32),
            renderer: Arc::new(Mutex::new(SceneRenderer::new(gl))),
            git_history: None,
            mesh_cache: HashMap::new(),
            cache_location,
            prefetch_rx: None,
            prefetch_attempted: HashSet::new(),
            idle_since: Instant::now(),
            max_cache_size_bytes,
        };
        if let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) {
            app.open_path(path, &cc.egui_ctx);
        }
        app
    }

    fn open_file(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .add_filter("OpenSCAD", &["scad"])
            .pick_file();
        if let Some(path) = picked {
            self.open_path(path, ctx);
        }
    }

    fn open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("scad") {
            self.status = "拡張子 .scad のファイルを選択してください".to_owned();
            return;
        }
        self.source_path = Some(path);
        self.git_history = self.source_path.as_deref().and_then(GitHistory::discover);
        self.mesh_cache.clear();
        self.prefetch_attempted.clear();
        self.idle_since = Instant::now();
        self.last_source_modified = None;
        self.reset_view();
        self.start_render(ctx);
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source_path.clone() else {
            return;
        };
        if self.is_rendering {
            self.render_pending = true;
            return;
        }

        let revision = self
            .git_history
            .as_ref()
            .and_then(GitHistory::selected_revision);
        let cache_root = self.cache_location.base_directory();
        let max_cache_size_bytes = self.max_cache_size_bytes;
        let cache_key = revision.as_ref().map(|revision| {
            format!(
                "{}:{}:{}",
                revision.root.display(),
                revision.relative_path.display(),
                revision.commit.hash
            )
        });
        if let Some(mesh) = cache_key
            .as_ref()
            .and_then(|key| self.mesh_cache.get(key))
            .cloned()
        {
            let faces = mesh.triangles.len();
            self.renderer.lock().set_mesh(&mesh);
            self.mesh = Some(mesh);
            self.last_source_modified = None;
            self.status = format!("{faces} 面をキャッシュから表示中");
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);
        self.is_rendering = true;
        self.idle_since = Instant::now();
        self.render_pending = false;
        self.status = revision.as_ref().map_or_else(
            || "OpenSCADでモデルを生成中…".to_owned(),
            |revision| {
                format!(
                    "{} ({}) を生成中…",
                    revision.commit.short_hash, revision.commit.date
                )
            },
        );
        let repaint = ctx.clone();

        thread::spawn(move || {
            let modified = revision
                .is_none()
                .then(|| source.metadata().and_then(|m| m.modified()).ok())
                .flatten();
            let (result, loaded_from_disk_cache) = if let Some(revision) = &revision {
                match load_or_render_revision(revision, cache_root.as_deref(), max_cache_size_bytes)
                {
                    Ok((mesh, from_cache)) => (Ok(mesh), from_cache),
                    Err(error) => (Err(error), false),
                }
            } else {
                (render_worktree(&source), false)
            };

            let message = match result {
                Ok(mesh) => RenderMessage::Finished {
                    mesh,
                    source_modified: modified,
                    cache_key,
                    loaded_from_disk_cache,
                },
                Err(error) => RenderMessage::Failed(error),
            };
            let _ = tx.send(message);
            repaint.request_repaint();
        });
    }

    fn receive_render(&mut self, ctx: &egui::Context) {
        let message = self.result_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(message) = message {
            self.is_rendering = false;
            self.result_rx = None;
            match message {
                RenderMessage::Finished {
                    mesh,
                    source_modified,
                    cache_key,
                    loaded_from_disk_cache,
                } => {
                    let faces = mesh.triangles.len();
                    self.renderer.lock().set_mesh(&mesh);
                    if let Some(cache_key) = cache_key {
                        self.mesh_cache.insert(cache_key, mesh.clone());
                    }
                    self.mesh = Some(mesh);
                    self.last_source_modified = source_modified;
                    self.status = if loaded_from_disk_cache {
                        format!("{faces} 面をディスクキャッシュから表示中")
                    } else {
                        format!("{faces} 面を表示中")
                    };
                }
                RenderMessage::Failed(error) => self.status = error,
            }
            if self.render_pending {
                self.start_render(ctx);
            } else {
                self.idle_since = Instant::now();
            }
        }
    }

    fn receive_prefetch(&mut self) {
        let finished = self.prefetch_rx.as_ref().is_some_and(|rx| {
            matches!(
                rx.try_recv(),
                Ok(()) | Err(mpsc::TryRecvError::Disconnected)
            )
        });
        if finished {
            self.prefetch_rx = None;
            self.idle_since = Instant::now();
        }
    }

    fn maybe_start_prefetch(&mut self, ctx: &egui::Context) {
        if self.is_rendering
            || self.render_pending
            || self.timeline_changed_at.is_some()
            || self.prefetch_rx.is_some()
        {
            return;
        }
        let idle_delay = Duration::from_millis(500);
        if self.idle_since.elapsed() < idle_delay {
            ctx.request_repaint_after(idle_delay - self.idle_since.elapsed());
            return;
        }
        let Some(cache_root) = self.cache_location.base_directory() else {
            return;
        };
        let max_cache_size_bytes = self.max_cache_size_bytes;
        let Some(history) = &self.git_history else {
            return;
        };

        let mut indices = Vec::with_capacity(history.commits.len());
        const PREFETCH_RADIUS: usize = 2;
        for distance in 1..=PREFETCH_RADIUS {
            if let Some(index) = history.position.checked_sub(distance)
                && index < history.commits.len()
            {
                indices.push(index);
            }
            let index = history.position + distance;
            if index < history.commits.len() {
                indices.push(index);
            }
        }

        let revision = indices.into_iter().find_map(|index| {
            let commit = history.commits.get(index)?.clone();
            let revision = GitRevision {
                root: history.root.clone(),
                relative_path: history.relative_path.clone(),
                commit,
            };
            let key = format!(
                "{}:{}:{}",
                revision.root.display(),
                revision.relative_path.display(),
                revision.commit.hash
            );
            if persistent_cache_path(&cache_root, &revision).is_file()
                || !self.prefetch_attempted.insert(key)
            {
                None
            } else {
                Some(revision)
            }
        });
        let Some(revision) = revision else {
            return;
        };

        let (tx, rx) = mpsc::channel();
        self.prefetch_rx = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            let _ = load_or_render_revision(&revision, Some(&cache_root), max_cache_size_bytes);
            let _ = tx.send(());
            repaint.request_repaint();
        });
    }

    fn watch_source(&mut self, ctx: &egui::Context) {
        let showing_worktree = self
            .git_history
            .as_ref()
            .is_none_or(|history| history.position == history.commits.len());
        if !self.auto_reload
            || !showing_worktree
            || self.is_rendering
            || self.last_watch_check.elapsed() < Duration::from_millis(500)
        {
            return;
        }
        self.last_watch_check = Instant::now();
        let modified = self
            .source_path
            .as_ref()
            .and_then(|p| p.metadata().ok())
            .and_then(|m| m.modified().ok());
        if modified.is_some() && modified != self.last_source_modified {
            self.start_render(ctx);
        }
    }

    fn reset_view(&mut self) {
        self.yaw = -0.65;
        self.pitch = 0.55;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    fn set_standard_view(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw;
        self.pitch = pitch;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};

        let pressed = |modifiers, key| {
            ctx.input_mut(|input| input.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
        };
        if pressed(Modifiers::CTRL, Key::O) {
            self.open_file(ctx);
        }
        if pressed(Modifiers::CTRL, Key::R) && self.source_path.is_some() {
            self.start_render(ctx);
        }
        if pressed(Modifiers::NONE, Key::Home) {
            self.reset_view();
        }
        if pressed(Modifiers::NONE, Key::Num0) {
            self.set_standard_view(-0.65, 0.55);
        }
        if pressed(Modifiers::NONE, Key::Num1) {
            self.set_standard_view(0.0, 0.0);
        }
        if pressed(Modifiers::NONE, Key::Num2) {
            self.set_standard_view(-std::f32::consts::FRAC_PI_2, 0.0);
        }
        if pressed(Modifiers::NONE, Key::Num3) {
            self.set_standard_view(0.0, 1.5);
        }
        if pressed(Modifiers::NONE, Key::E) {
            self.show_edges = !self.show_edges;
        }
        if pressed(Modifiers::NONE, Key::A) {
            self.show_axes = !self.show_axes;
        }
    }

    fn rotate_vertex(&self, vertex: Vec3, center: Vec3) -> Vec3 {
        let p = vertex.sub(center);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let x1 = cy * p.x + sy * p.z;
        let z1 = -sy * p.x + cy * p.z;
        Vec3::new(x1, cp * p.y - sp * z1, sp * p.y + cp * z1)
    }

    fn draw_view_cube(&mut self, painter: &Painter, cube_rect: Rect, response: &egui::Response) {
        struct Face {
            points: [Pos2; 4],
            depth: f32,
            label: &'static str,
            target: [f32; 2],
        }

        let vertices = [
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let rotated: Vec<Vec3> = vertices
            .iter()
            .map(|vertex| self.rotate_vertex(*vertex, Vec3::default()))
            .collect();
        let definitions = [
            ([4, 5, 6, 7], Vec3::new(0.0, 0.0, 1.0), "前", [0.0, 0.0]),
            (
                [1, 0, 3, 2],
                Vec3::new(0.0, 0.0, -1.0),
                "後",
                [std::f32::consts::PI, 0.0],
            ),
            (
                [5, 1, 2, 6],
                Vec3::new(1.0, 0.0, 0.0),
                "右",
                [-std::f32::consts::FRAC_PI_2, 0.0],
            ),
            (
                [0, 4, 7, 3],
                Vec3::new(-1.0, 0.0, 0.0),
                "左",
                [std::f32::consts::FRAC_PI_2, 0.0],
            ),
            ([7, 6, 2, 3], Vec3::new(0.0, 1.0, 0.0), "上", [0.0, 1.5]),
            ([0, 1, 5, 4], Vec3::new(0.0, -1.0, 0.0), "下", [0.0, -1.5]),
        ];
        let center = cube_rect.center();
        let scale = cube_rect.width() * 0.24;
        let mut faces = Vec::new();
        for (indices, normal, label, target) in definitions {
            if self.rotate_vertex(normal, Vec3::default()).z <= 0.01 {
                continue;
            }
            faces.push(Face {
                points: indices.map(|index| {
                    let point = rotated[index];
                    center + Vec2::new(point.x * scale, -point.y * scale)
                }),
                depth: indices.iter().map(|index| rotated[*index].z).sum::<f32>() / 4.0,
                label,
                target,
            });
        }
        faces.sort_by(|a, b| a.depth.total_cmp(&b.depth));

        let pointer = response.hover_pos();
        let hovered = pointer.and_then(|pointer| {
            faces
                .iter()
                .rposition(|face| point_in_convex_polygon(pointer, &face.points))
        });
        painter.rect_filled(cube_rect, 8.0, Color32::from_black_alpha(105));
        for (index, face) in faces.iter().enumerate() {
            let fill = if Some(index) == hovered {
                Color32::from_rgb(95, 150, 190)
            } else {
                Color32::from_rgb(58, 78, 92)
            };
            painter.add(egui::Shape::convex_polygon(
                face.points.to_vec(),
                fill,
                Stroke::new(1.2_f32, Color32::from_gray(190)),
            ));
            let face_center = face
                .points
                .iter()
                .fold(Vec2::ZERO, |sum, point| sum + point.to_vec2())
                / 4.0;
            painter.text(
                Pos2::new(face_center.x, face_center.y),
                Align2::CENTER_CENTER,
                face.label,
                FontId::proportional(10.0),
                Color32::WHITE,
            );
        }
        if response.clicked()
            && let Some(index) = hovered
        {
            let target = faces[index].target;
            self.set_standard_view(target[0], target[1]);
            response.ctx.request_repaint();
        }
    }

    fn draw_git_timeline(&mut self, ctx: &egui::Context) {
        let cache_root = self.cache_location.base_directory();
        let is_prefetching = self.prefetch_rx.is_some();
        let Some(history) = &mut self.git_history else {
            return;
        };
        let mut selection_changed = false;

        egui::TopBottomPanel::bottom("git_timeline")
            .resizable(false)
            .exact_height(100.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Git タイムライン");
                    ui.separator();
                    ui.label("ブランチ");
                    let previous_branch = history.selected_branch;
                    egui::ComboBox::from_id_salt("git_branch")
                        .selected_text(
                            history
                                .branches
                                .get(history.selected_branch)
                                .map_or("不明", String::as_str),
                        )
                        .show_ui(ui, |ui| {
                            for (index, branch) in history.branches.iter().enumerate() {
                                ui.selectable_value(&mut history.selected_branch, index, branch);
                            }
                        });
                    if history.selected_branch != previous_branch {
                        history.load_commits();
                        if !history.commits.is_empty() {
                            history.position = history.commits.len() - 1;
                        }
                        selection_changed = true;
                    }
                    ui.separator();
                    ui.label(format!("{} commits", history.commits.len()));
                    if is_prefetching {
                        ui.spinner();
                        ui.label("周辺を先読み中");
                    }
                });

                let max_position = history.commits.len();
                let (timeline_rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 32.0),
                    Sense::click_and_drag(),
                );
                let left = timeline_rect.left() + 8.0;
                let right = timeline_rect.right() - 8.0;
                let center_y = timeline_rect.center().y;
                ui.painter().line_segment(
                    [Pos2::new(left, center_y), Pos2::new(right, center_y)],
                    Stroke::new(2.0_f32, Color32::from_gray(75)),
                );

                for index in 0..=max_position {
                    let fraction = if max_position == 0 {
                        1.0
                    } else {
                        index as f32 / max_position as f32
                    };
                    let position = Pos2::new(egui::lerp(left..=right, fraction), center_y);
                    let is_selected = index == history.position;
                    let is_worktree = index == max_position;
                    let is_cached = history.commits.get(index).is_some_and(|commit| {
                        cache_root.as_deref().is_some_and(|cache_root| {
                            persistent_cache_path(
                                cache_root,
                                &GitRevision {
                                    root: history.root.clone(),
                                    relative_path: history.relative_path.clone(),
                                    commit: commit.clone(),
                                },
                            )
                            .is_file()
                        })
                    });
                    let color = if is_selected {
                        Color32::from_rgb(255, 190, 70)
                    } else if is_worktree {
                        Color32::from_rgb(90, 210, 130)
                    } else if is_cached {
                        Color32::from_rgb(75, 165, 235)
                    } else {
                        Color32::from_gray(155)
                    };
                    ui.painter().circle_filled(
                        position,
                        if is_selected { 6.0 } else { 3.5 },
                        color,
                    );
                }

                if (response.clicked() || response.dragged())
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let fraction = ((pointer.x - left) / (right - left)).clamp(0.0, 1.0);
                    let new_position = (fraction * max_position as f32).round() as usize;
                    if new_position != history.position {
                        history.position = new_position;
                        selection_changed = true;
                    }
                }

                if history.position == history.commits.len() {
                    ui.horizontal(|ui| {
                        ui.strong("作業ツリー");
                        ui.label("現在編集中のファイル");
                    });
                } else if let Some(commit) = history.commits.get(history.position) {
                    ui.horizontal(|ui| {
                        ui.monospace(&commit.short_hash);
                        ui.label(&commit.date);
                        ui.label(&commit.subject);
                    });
                } else {
                    ui.label("このブランチには対象ファイルの履歴がありません");
                }
            });

        if selection_changed {
            self.timeline_changed_at = Some(Instant::now());
            self.idle_since = Instant::now();
            self.status = "コミットを選択中…".to_owned();
            ctx.request_repaint_after(Duration::from_millis(160));
        }
    }

    fn draw_viewport(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, self.background);

        let cube_size = rect.width().min(rect.height()).clamp(76.0, 96.0);
        let cube_rect = Rect::from_min_size(
            Pos2::new(rect.right() - cube_size - 12.0, rect.top() + 12.0),
            Vec2::splat(cube_size),
        );
        let cube_response = ui.interact(
            cube_rect,
            ui.id().with("view_cube"),
            Sense::click_and_drag(),
        );

        if cube_response.dragged_by(egui::PointerButton::Primary) {
            let delta = cube_response.drag_motion();
            self.yaw += delta.x * 0.008;
            self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.5, 1.5);
            ui.ctx().request_repaint();
        } else if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_motion();
            self.yaw += delta.x * 0.008;
            self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.5, 1.5);
            ui.ctx().request_repaint();
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
        {
            self.pan += response.drag_motion();
            ui.ctx().request_repaint();
        }
        if response.hovered() && !cube_response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (scroll * 0.0015).exp()).clamp(0.05, 40.0);
                ui.ctx().request_repaint();
            }
        }
        if response.double_clicked() {
            self.reset_view();
        }

        if self.mesh.is_none() {
            self.draw_empty_state(&painter, rect);
            self.draw_view_cube(&painter, cube_rect, &cube_response);
            return;
        }

        let origin = rect.center() + self.pan;
        let yaw = self.yaw;
        let pitch = self.pitch;
        let zoom = self.zoom;
        let pan = self.pan;
        let show_edges = self.show_edges;
        let renderer = Arc::clone(&self.renderer);
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let viewport = info.viewport_in_pixels();
                renderer.lock().paint(
                    painter.gl(),
                    ScenePaintParams {
                        angles: [yaw, pitch],
                        zoom,
                        pan: [pan.x / (rect.width() * 0.5), -pan.y / (rect.height() * 0.5)],
                        aspect: rect.width() / rect.height(),
                        show_edges,
                        viewport: [
                            viewport.left_px,
                            viewport.from_bottom_px,
                            viewport.width_px,
                            viewport.height_px,
                        ],
                        destination: painter.intermediate_fbo(),
                    },
                );
            })),
        });

        if self.show_axes {
            let scale = rect.width().min(rect.height()) * 0.075;
            self.draw_axes(&painter, origin, scale, 1.0);
        }
        self.draw_view_cube(&painter, cube_rect, &cube_response);
    }

    fn draw_empty_state(&self, painter: &Painter, rect: Rect) {
        painter.text(
            rect.center() - Vec2::new(0.0, 18.0),
            Align2::CENTER_CENTER,
            "SCAD",
            FontId::proportional(32.0),
            Color32::from_gray(105),
        );
        painter.text(
            rect.center() + Vec2::new(0.0, 22.0),
            Align2::CENTER_CENTER,
            "「開く」から .scad ファイルを選択",
            FontId::proportional(15.0),
            Color32::from_gray(145),
        );
    }

    fn draw_axes(&self, painter: &Painter, origin: Pos2, scale: f32, radius: f32) {
        let length = radius * 0.65;
        let axes = [
            (
                Vec3::new(length, 0.0, 0.0),
                Color32::from_rgb(235, 80, 80),
                "X",
            ),
            (
                Vec3::new(0.0, length, 0.0),
                Color32::from_rgb(90, 220, 110),
                "Y",
            ),
            (
                Vec3::new(0.0, 0.0, length),
                Color32::from_rgb(90, 145, 245),
                "Z",
            ),
        ];
        for (endpoint, color, label) in axes {
            let rotated = self.rotate_vertex(endpoint, Vec3::default());
            let end = origin + Vec2::new(rotated.x * scale, -rotated.y * scale);
            painter.line_segment([origin, end], Stroke::new(1.5_f32, color));
            painter.text(
                end,
                Align2::CENTER_CENTER,
                label,
                FontId::monospace(11.0),
                color,
            );
        }
    }
}

impl eframe::App for ScadlineApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(ctx);
        self.receive_render(ctx);
        self.receive_prefetch();
        self.watch_source(ctx);
        if let Some(changed_at) = self.timeline_changed_at {
            let debounce = Duration::from_millis(160);
            if changed_at.elapsed() >= debounce {
                self.timeline_changed_at = None;
                self.start_render(ctx);
            } else {
                ctx.request_repaint_after(debounce - changed_at.elapsed());
            }
        }
        if !self.is_rendering {
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .first()
                    .and_then(|file| file.path.clone())
            });
            if let Some(path) = dropped {
                self.open_path(path, ctx);
            }
        }
        if self.is_rendering {
            ctx.request_repaint_after(Duration::from_millis(80));
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("📂  開く")
                    .on_hover_text("SCADファイルを開く (Ctrl+O)")
                    .clicked()
                {
                    self.open_file(ctx);
                }
                if ui
                    .add_enabled(
                        self.source_path.is_some() && !self.is_rendering,
                        egui::Button::new("↻  再読込"),
                    )
                    .on_hover_text("現在のファイルを再生成 (Ctrl+R)")
                    .clicked()
                {
                    self.start_render(ctx);
                }
                if ui
                    .button("⌂  表示を戻す")
                    .on_hover_text("表示を初期位置へ戻す (Home / 0)")
                    .clicked()
                {
                    self.reset_view();
                }
                ui.separator();
                ui.checkbox(&mut self.auto_reload, "自動更新");
                ui.checkbox(&mut self.show_edges, "エッジ")
                    .on_hover_text("エッジ表示を切り替え (E)");
                ui.checkbox(&mut self.show_axes, "軸")
                    .on_hover_text("座標軸を切り替え (A)");
                if self.is_rendering {
                    ui.spinner();
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let name = self
                    .source_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("ファイル未選択");
                ui.label(name);
                ui.separator();
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("左ドラッグ: 回転  右/中ドラッグ: 移動  ホイール: ズーム");
                });
            });
        });

        self.draw_git_timeline(ctx);

        egui::CentralPanel::default().show(ctx, |ui| self.draw_viewport(ui));
        self.maybe_start_prefetch(ctx);
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            self.renderer.lock().destroy(gl);
        }
    }
}

fn configure_japanese_fonts(ctx: &egui::Context) {
    const FONT_PATHS: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansJP-Regular.otf",
        "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
    ];
    let Some(bytes) = FONT_PATHS.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-cjk".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "noto-cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

struct SceneRenderer {
    program: glow::Program,
    vertex_array: glow::VertexArray,
    vertex_buffer: glow::Buffer,
    framebuffer: glow::Framebuffer,
    color_buffer: glow::Renderbuffer,
    depth_buffer: glow::Renderbuffer,
    render_size: [i32; 2],
    cpu_vertices: Vec<f32>,
    vertex_count: i32,
    dirty: bool,
}

struct ScenePaintParams {
    angles: [f32; 2],
    zoom: f32,
    pan: [f32; 2],
    aspect: f32,
    show_edges: bool,
    viewport: [i32; 4],
    destination: Option<glow::Framebuffer>,
}

impl SceneRenderer {
    fn new(gl: &glow::Context) -> Self {
        unsafe {
            let program = gl.create_program().expect("OpenGL program creation failed");
            let shader_sources = [
                (
                    glow::VERTEX_SHADER,
                    r#"#version 330
                    layout(location = 0) in vec3 a_position;
                    layout(location = 1) in vec3 a_normal;
                    uniform vec2 u_angles;
                    uniform vec2 u_scale;
                    uniform vec2 u_pan;
                    out vec3 v_normal;

                    vec3 rotate_model(vec3 p) {
                        float sy = sin(u_angles.x), cy = cos(u_angles.x);
                        float sp = sin(u_angles.y), cp = cos(u_angles.y);
                        vec3 y = vec3(cy*p.x + sy*p.z, p.y, -sy*p.x + cy*p.z);
                        return vec3(y.x, cp*y.y - sp*y.z, sp*y.y + cp*y.z);
                    }

                    void main() {
                        vec3 p = rotate_model(a_position);
                        v_normal = rotate_model(a_normal);
                        gl_Position = vec4(p.xy * u_scale + u_pan, -p.z * 0.25, 1.0);
                    }"#,
                ),
                (
                    glow::FRAGMENT_SHADER,
                    r#"#version 330
                    in vec3 v_normal;
                    uniform bool u_wire;
                    out vec4 out_color;
                    void main() {
                        if (u_wire) {
                            out_color = vec4(0.035, 0.045, 0.055, 0.72);
                            return;
                        }
                        vec3 light = normalize(vec3(-0.35, 0.55, 1.0));
                        float shade = 0.30 + 0.70 * abs(dot(normalize(v_normal), light));
                        out_color = vec4(vec3(0.22, 0.62, 0.78) * shade + vec3(0.06), 1.0);
                    }"#,
                ),
            ];
            let mut shaders = Vec::new();
            for (kind, source) in shader_sources {
                let shader = gl
                    .create_shader(kind)
                    .expect("OpenGL shader creation failed");
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                assert!(
                    gl.get_shader_compile_status(shader),
                    "OpenGL shader error: {}",
                    gl.get_shader_info_log(shader)
                );
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            assert!(
                gl.get_program_link_status(program),
                "OpenGL link error: {}",
                gl.get_program_info_log(program)
            );
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }

            let vertex_array = gl
                .create_vertex_array()
                .expect("OpenGL VAO creation failed");
            let vertex_buffer = gl.create_buffer().expect("OpenGL buffer creation failed");
            let framebuffer = gl
                .create_framebuffer()
                .expect("OpenGL framebuffer creation failed");
            let color_buffer = gl
                .create_renderbuffer()
                .expect("OpenGL color buffer creation failed");
            let depth_buffer = gl
                .create_renderbuffer()
                .expect("OpenGL depth buffer creation failed");
            gl.bind_vertex_array(Some(vertex_array));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertex_buffer));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 24, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, 24, 12);

            Self {
                program,
                vertex_array,
                vertex_buffer,
                framebuffer,
                color_buffer,
                depth_buffer,
                render_size: [0, 0],
                cpu_vertices: Vec::new(),
                vertex_count: 0,
                dirty: false,
            }
        }
    }

    fn set_mesh(&mut self, mesh: &Mesh) {
        self.cpu_vertices.clear();
        self.cpu_vertices.reserve(mesh.triangles.len() * 18);
        for triangle in &mesh.triangles {
            let a = mesh.vertices[triangle[0]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let b = mesh.vertices[triangle[1]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let c = mesh.vertices[triangle[2]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let normal = b.sub(a).cross(c.sub(a)).normalized();
            for vertex in [a, b, c] {
                self.cpu_vertices.extend_from_slice(&[
                    vertex.x, vertex.y, vertex.z, normal.x, normal.y, normal.z,
                ]);
            }
        }
        self.vertex_count = (self.cpu_vertices.len() / 6) as i32;
        self.dirty = true;
    }

    fn paint(&mut self, gl: &glow::Context, params: ScenePaintParams) {
        let ScenePaintParams {
            angles,
            zoom,
            pan,
            aspect,
            show_edges,
            viewport,
            destination,
        } = params;
        unsafe {
            let width = viewport[2].max(1);
            let height = viewport[3].max(1);
            self.ensure_render_target(gl, width, height);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.viewport(0, 0, width, height);
            gl.disable(glow::SCISSOR_TEST);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.disable(glow::BLEND);
            gl.clear_color(24.0 / 255.0, 27.0 / 255.0, 32.0 / 255.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertex_array));
            if self.dirty {
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck::cast_slice(&self.cpu_vertices),
                    glow::STATIC_DRAW,
                );
                self.dirty = false;
            }

            let scale = if aspect >= 1.0 {
                [0.84 * zoom / aspect, 0.84 * zoom]
            } else {
                [0.84 * zoom, 0.84 * zoom * aspect]
            };
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_angles").as_ref(),
                angles[0],
                angles[1],
            );
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_scale").as_ref(),
                scale[0],
                scale[1],
            );
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_pan").as_ref(),
                pan[0],
                pan[1],
            );
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "u_wire").as_ref(), 0);

            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1.0, 1.0);
            gl.draw_arrays(glow::TRIANGLES, 0, self.vertex_count);
            gl.disable(glow::POLYGON_OFFSET_FILL);

            if show_edges {
                gl.uniform_1_i32(gl.get_uniform_location(self.program, "u_wire").as_ref(), 1);
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::LINE);
                gl.line_width(1.0);
                gl.draw_arrays(glow::TRIANGLES, 0, self.vertex_count);
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
            }
            gl.disable(glow::DEPTH_TEST);

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, destination);
            gl.blit_framebuffer(
                0,
                0,
                width,
                height,
                viewport[0],
                viewport[1],
                viewport[0] + width,
                viewport[1] + height,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, destination);
        }
    }

    fn ensure_render_target(&mut self, gl: &glow::Context, width: i32, height: i32) {
        if self.render_size == [width, height] {
            return;
        }
        self.render_size = [width, height];

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.color_buffer));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, width, height);
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(self.color_buffer),
            );
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.depth_buffer));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, width, height);
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(self.depth_buffer),
            );
            assert_eq!(
                gl.check_framebuffer_status(glow::FRAMEBUFFER),
                glow::FRAMEBUFFER_COMPLETE,
                "OpenGL framebuffer is incomplete"
            );
        }
    }

    fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vertex_array);
            gl.delete_buffer(self.vertex_buffer);
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_renderbuffer(self.color_buffer);
            gl.delete_renderbuffer(self.depth_buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_uses_default_cache_limit_when_omitted() {
        let config: AppConfig = toml::from_str("").expect("empty config should use defaults");
        assert_eq!(config.cache.max_size_mb, DEFAULT_CACHE_SIZE_MB);
    }

    #[test]
    fn cache_pruning_keeps_total_under_limit() {
        let directory =
            std::env::temp_dir().join(format!("scadline-cache-test-{}", unique_render_token()));
        std::fs::create_dir_all(directory.join("repo/commit")).expect("create cache fixture");
        for index in 0..3 {
            std::fs::write(
                directory.join(format!("repo/commit/{index}.stl")),
                [index as u8; 10],
            )
            .expect("write cache fixture");
        }

        enforce_cache_size(&directory, 15);

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
        std::fs::remove_dir_all(directory).expect("remove cache fixture");
    }
}
