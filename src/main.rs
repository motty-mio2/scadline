use std::{
    fs::File,
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
    },
    Failed(String),
}

struct ScadlineApp {
    source_path: Option<PathBuf>,
    mesh: Option<Mesh>,
    result_rx: Option<Receiver<RenderMessage>>,
    is_rendering: bool,
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
}

impl ScadlineApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_japanese_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let gl = cc.gl.as_ref().expect("Glow renderer is required");
        let mut app = Self {
            source_path: None,
            mesh: None,
            result_rx: None,
            is_rendering: false,
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
        self.last_source_modified = None;
        self.start_render(ctx);
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        let Some(source) = self.source_path.clone() else {
            return;
        };
        if self.is_rendering {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);
        self.is_rendering = true;
        self.status = "OpenSCADでモデルを生成中…".to_owned();
        let repaint = ctx.clone();

        thread::spawn(move || {
            let modified = source.metadata().and_then(|m| m.modified()).ok();
            let output_path = std::env::temp_dir().join(format!(
                "scadline-{}-{}.stl",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));

            let result = Command::new("openscad")
                .arg("--export-format")
                .arg("binstl")
                .arg("-o")
                .arg(&output_path)
                .arg(&source)
                .output()
                .map_err(|e| {
                    format!(
                        "OpenSCADを起動できません: {e}\nOpenSCADがインストールされているか確認してください。"
                    )
                })
                .and_then(|output| {
                    if output.status.success() {
                        Mesh::from_stl(&output_path)
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        Err(format!("OpenSCADエラー:\n{}", stderr.trim()))
                    }
                });
            let _ = std::fs::remove_file(&output_path);

            let message = match result {
                Ok(mesh) => RenderMessage::Finished {
                    mesh,
                    source_modified: modified,
                },
                Err(error) => RenderMessage::Failed(error),
            };
            let _ = tx.send(message);
            repaint.request_repaint();
        });
    }

    fn receive_render(&mut self) {
        let message = self.result_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(message) = message {
            self.is_rendering = false;
            self.result_rx = None;
            match message {
                RenderMessage::Finished {
                    mesh,
                    source_modified,
                } => {
                    let faces = mesh.triangles.len();
                    self.renderer.lock().set_mesh(&mesh);
                    self.mesh = Some(mesh);
                    self.last_source_modified = source_modified;
                    self.status = format!("{faces} 面を表示中");
                    self.reset_view();
                }
                RenderMessage::Failed(error) => self.status = error,
            }
        }
    }

    fn watch_source(&mut self, ctx: &egui::Context) {
        if !self.auto_reload
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

    fn rotate_vertex(&self, vertex: Vec3, center: Vec3) -> Vec3 {
        let p = vertex.sub(center);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let x1 = cy * p.x + sy * p.z;
        let z1 = -sy * p.x + cy * p.z;
        Vec3::new(x1, cp * p.y - sp * z1, sp * p.y + cp * z1)
    }

    fn draw_viewport(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, self.background);

        if response.dragged_by(egui::PointerButton::Primary) {
            let delta = response.drag_delta();
            self.yaw += delta.x * 0.008;
            self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.5, 1.5);
            ui.ctx().request_repaint();
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
        {
            self.pan += response.drag_delta();
            ui.ctx().request_repaint();
        }
        if response.hovered() {
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
                    [yaw, pitch],
                    zoom,
                    [pan.x / (rect.width() * 0.5), -pan.y / (rect.height() * 0.5)],
                    rect.width() / rect.height(),
                    show_edges,
                    [
                        viewport.left_px,
                        viewport.from_bottom_px,
                        viewport.width_px,
                        viewport.height_px,
                    ],
                    painter.intermediate_fbo(),
                );
            })),
        });

        if self.show_axes {
            let scale = rect.width().min(rect.height()) * 0.075;
            self.draw_axes(&painter, origin, scale, 1.0);
        }
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
            painter.line_segment([origin, end], Stroke::new(1.5, color));
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
        self.receive_render();
        self.watch_source(ctx);
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
                if ui.button("📂  開く").clicked() {
                    self.open_file(ctx);
                }
                if ui
                    .add_enabled(
                        self.source_path.is_some() && !self.is_rendering,
                        egui::Button::new("↻  再読込"),
                    )
                    .clicked()
                {
                    self.start_render(ctx);
                }
                if ui.button("⌂  表示を戻す").clicked() {
                    self.reset_view();
                }
                ui.separator();
                ui.checkbox(&mut self.auto_reload, "自動更新");
                ui.checkbox(&mut self.show_edges, "エッジ");
                ui.checkbox(&mut self.show_axes, "軸");
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

        egui::CentralPanel::default().show(ctx, |ui| self.draw_viewport(ui));
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

    fn paint(
        &mut self,
        gl: &glow::Context,
        angles: [f32; 2],
        zoom: f32,
        pan: [f32; 2],
        aspect: f32,
        show_edges: bool,
        viewport: [i32; 4],
        destination: Option<glow::Framebuffer>,
    ) {
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
