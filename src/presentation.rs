mod renderer;
mod view;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    application::{ModelService, RenderOutcome},
    cli::CliOptions,
    domain::{GitHistory, GitRevision, Mesh, Vec3},
    infrastructure::{
        CacheLocation, GitRepository, ModelLoader, PlatformCacheLocation, load_or_create_config,
    },
};
use eframe::egui;
use eframe::egui::{Color32, Pos2, Vec2};
use egui::mutex::Mutex;

use renderer::SceneRenderer;

pub(crate) fn run(cli: CliOptions) -> eframe::Result<()> {
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
        Box::new(move |cc| Ok(Box::new(AppState::new(cc, cli)))),
    )
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

struct AppState {
    source_path: Option<PathBuf>,
    mesh: Option<Mesh>,
    result_rx: Option<Receiver<RenderOutcome>>,
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
    model_service: Arc<ModelService>,
    prefetch_rx: Option<Receiver<()>>,
    prefetch_attempted: HashSet<String>,
    idle_since: Instant,
}

enum UiAction {
    OpenFile,
    OpenPath(PathBuf),
    Reload,
    ResetView,
    SetStandardView { yaw: f32, pitch: f32 },
    ToggleEdges,
    ToggleAxes,
}

impl AppState {
    fn new(cc: &eframe::CreationContext<'_>, cli: CliOptions) -> Self {
        Self::new_with_cache_location(cc, Arc::new(PlatformCacheLocation), cli)
    }

    fn new_with_cache_location(
        cc: &eframe::CreationContext<'_>,
        cache_location: Arc<dyn CacheLocation>,
        cli: CliOptions,
    ) -> Self {
        let config = load_or_create_config();
        let max_cache_size_bytes = config.cache.max_size_mb.saturating_mul(1024 * 1024);
        let backend = cli.backend.or(config.openscad.backend);
        let model_loader = Arc::new(ModelLoader::new(
            cache_location,
            max_cache_size_bytes,
            backend,
        ));
        let model_service = Arc::new(ModelService::new(model_loader));
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
            model_service,
            prefetch_rx: None,
            prefetch_attempted: HashSet::new(),
            idle_since: Instant::now(),
        };
        if let Some(path) = cli.source_path {
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

    fn dispatch(&mut self, action: UiAction, ctx: &egui::Context) {
        match action {
            UiAction::OpenFile => self.open_file(ctx),
            UiAction::OpenPath(path) => self.open_path(path, ctx),
            UiAction::Reload if self.source_path.is_some() => self.start_render(ctx),
            UiAction::Reload => {}
            UiAction::ResetView => self.reset_view(),
            UiAction::SetStandardView { yaw, pitch } => self.set_standard_view(yaw, pitch),
            UiAction::ToggleEdges => self.show_edges = !self.show_edges,
            UiAction::ToggleAxes => self.show_axes = !self.show_axes,
        }
    }

    fn open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("scad") {
            self.status = "拡張子 .scad のファイルを選択してください".to_owned();
            return;
        }
        self.source_path = Some(path);
        self.git_history = self
            .source_path
            .as_deref()
            .and_then(GitRepository::discover);
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
        let model_service = Arc::clone(&self.model_service);
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
            let message = model_service.render(&source, revision.as_ref(), cache_key);
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
                RenderOutcome::Finished {
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
                RenderOutcome::Failed(error) => self.status = error,
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
            if self.model_service.is_cached(&revision) || !self.prefetch_attempted.insert(key) {
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
        let model_service = Arc::clone(&self.model_service);
        thread::spawn(move || {
            model_service.prefetch(&revision);
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
            self.dispatch(UiAction::OpenFile, ctx);
        }
        if pressed(Modifiers::CTRL, Key::R) {
            self.dispatch(UiAction::Reload, ctx);
        }
        if pressed(Modifiers::NONE, Key::Home) {
            self.dispatch(UiAction::ResetView, ctx);
        }
        if pressed(Modifiers::NONE, Key::Num0) {
            self.dispatch(
                UiAction::SetStandardView {
                    yaw: -0.65,
                    pitch: 0.55,
                },
                ctx,
            );
        }
        if pressed(Modifiers::NONE, Key::Num1) {
            self.dispatch(
                UiAction::SetStandardView {
                    yaw: 0.0,
                    pitch: 0.0,
                },
                ctx,
            );
        }
        if pressed(Modifiers::NONE, Key::Num2) {
            self.dispatch(
                UiAction::SetStandardView {
                    yaw: -std::f32::consts::FRAC_PI_2,
                    pitch: 0.0,
                },
                ctx,
            );
        }
        if pressed(Modifiers::NONE, Key::Num3) {
            self.dispatch(
                UiAction::SetStandardView {
                    yaw: 0.0,
                    pitch: 1.5,
                },
                ctx,
            );
        }
        if pressed(Modifiers::NONE, Key::E) {
            self.dispatch(UiAction::ToggleEdges, ctx);
        }
        if pressed(Modifiers::NONE, Key::A) {
            self.dispatch(UiAction::ToggleAxes, ctx);
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
}

fn configure_japanese_fonts(ctx: &egui::Context) {
    #[cfg(target_os = "linux")]
    const FONT_PATHS: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansJP-Regular.otf",
        "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
    ];
    #[cfg(target_os = "macos")]
    const FONT_PATHS: &[&str] = &[
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    ];
    #[cfg(target_os = "windows")]
    const FONT_PATHS: &[&str] = &[
        "C:\\Windows\\Fonts\\YuGothR.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    const FONT_PATHS: &[&str] = &[];
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
