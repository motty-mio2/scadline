use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    domain::{GitRevision, Vec3},
    infrastructure::GitRepository,
};
use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::{egui, egui_glow, glow};

use super::UiAction;
use super::{AppState, point_in_convex_polygon, renderer::ScenePaintParams};

impl AppState {
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
            self.dispatch(
                UiAction::SetStandardView {
                    yaw: target[0],
                    pitch: target[1],
                },
                &response.ctx,
            );
            response.ctx.request_repaint();
        }
    }

    fn draw_git_timeline(&mut self, ctx: &egui::Context) {
        let model_service = Arc::clone(&self.model_service);
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
                        GitRepository::load_commits(history);
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
                        model_service.is_cached(&GitRevision {
                            root: history.root.clone(),
                            relative_path: history.relative_path.clone(),
                            commit: commit.clone(),
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

impl eframe::App for AppState {
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
                self.dispatch(UiAction::OpenPath(path), ctx);
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
                    self.dispatch(UiAction::OpenFile, ctx);
                }
                if ui
                    .add_enabled(
                        self.source_path.is_some() && !self.is_rendering,
                        egui::Button::new("↻  再読込"),
                    )
                    .on_hover_text("現在のファイルを再生成 (Ctrl+R)")
                    .clicked()
                {
                    self.dispatch(UiAction::Reload, ctx);
                }
                if ui
                    .button("⌂  表示を戻す")
                    .on_hover_text("表示を初期位置へ戻す (Home / 0)")
                    .clicked()
                {
                    self.dispatch(UiAction::ResetView, ctx);
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
