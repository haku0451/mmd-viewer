//! Japanese egui shell around the GPU viewport.

use egui::Color32;
use mmd_editor::{CameraDrag, CameraInput, CameraPointerButton, EditorCamera};
use mmd_renderer::AdapterSummary;

use crate::session::Session;

/// Result of one egui frame.
pub struct UiOutput {
    pub close_requested: bool,
    /// Physical viewport size requested by the central panel.
    pub viewport_pixels: Option<(u32, u32)>,
}

/// Draw the viewer shell and apply viewport camera input.
pub fn show(
    ui: &mut egui::Ui,
    camera: &mut EditorCamera,
    summary: &AdapterSummary,
    session: &mut Session,
    texture_id: Option<egui::TextureId>,
) -> UiOutput {
    let mut output = UiOutput {
        close_requested: false,
        viewport_pixels: None,
    };

    egui::Panel::top("menu").show(ui, |ui| {
        egui::containers::menu::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("ファイル", |ui| {
                if ui.button("モデルを追加").clicked() {
                    session.open(mmd_platform_windows::OpenFileKind::Model);
                    ui.close();
                }
                if ui.button("モーションを適用").clicked() {
                    session.open(mmd_platform_windows::OpenFileKind::Motion);
                    ui.close();
                }
                if ui.button("音声を開く").clicked() {
                    session.open(mmd_platform_windows::OpenFileKind::Audio);
                    ui.close();
                }
                if ui.button("選択中のモデルを外す").clicked() {
                    session.remove_selected();
                    ui.close();
                }
                if ui.button("終了").clicked() {
                    output.close_requested = true;
                    ui.close();
                }
            });
            ui.menu_button("表示", |ui| {
                if ui.button("カメラをリセット").clicked() {
                    apply_camera_input(camera, CameraInput::Reset, 1.0);
                    ui.close();
                }
                let follow = if session.follow_camera {
                    "カメラモーションに従う ✓"
                } else {
                    "カメラモーションに従う"
                };
                if ui.button(follow).clicked() {
                    session.follow_camera = !session.follow_camera;
                    ui.close();
                }
                ui.checkbox(&mut session.ground_shadow, "地面影");
                ui.checkbox(&mut session.self_shadow, "セルフシャドウ");
                ui.separator();
                let mut physics = session.physics_enabled;
                if ui.checkbox(&mut physics, "物理ON/OFF").changed() {
                    session.set_physics_enabled(physics);
                }
                if ui.button("物理リセット").clicked() {
                    session.reset_physics();
                    ui.close();
                }
                ui.separator();
                for rate in [
                    crate::session::FrameRate::Fps30,
                    crate::session::FrameRate::Fps60,
                    crate::session::FrameRate::Unlimited,
                ] {
                    let label = if session.frame_rate == rate {
                        format!("{} ✓", rate.label())
                    } else {
                        rate.label().to_string()
                    };
                    if ui.button(label).clicked() {
                        session.frame_rate = rate;
                        ui.close();
                    }
                }
            });
        });
    });

    egui::Panel::left("status")
        .default_size(300.0)
        .show(ui, |ui| {
            ui.heading("MMD Viewer");
            ui.label(session.message.as_str());
            ui.separator();
            playback(ui, session);
            ui.separator();
            model_summary(ui, session);
            ui.separator();
            ui.label(format!("アダプタ: {}", summary.name));
            ui.label(format!("バックエンド: {}", summary.backend));
            ui.label(format!("デバイス: {}", summary.device_type));
            if summary.dx12 {
                ui.label("Direct3D 12 を使用中");
            } else if summary.used_fallback_backends {
                ui.colored_label(
                    Color32::from_rgb(214, 168, 92),
                    "Direct3D 12 を初期化できなかったため、別のバックエンドを使用中",
                );
            } else {
                ui.colored_label(
                    Color32::from_rgb(214, 168, 92),
                    "Direct3D 12 は選択されていません",
                );
            }
            ui.separator();
            ui.label("右ドラッグ: 回転");
            ui.label("中ドラッグ: 平行移動");
            ui.label("ホイール: ズーム");
            ui.label("Home: カメラをリセット");
            if ui.button("カメラをリセット").clicked() {
                apply_camera_input(camera, CameraInput::Reset, 1.0);
            }
            let eye = camera.eye();
            let target = camera.target();
            ui.label(format!("視点 {:.2} {:.2} {:.2}", eye.x, eye.y, eye.z));
            ui.label(format!(
                "注視点 {:.2} {:.2} {:.2}",
                target.x, target.y, target.z
            ));
            ui.label(format!("距離 {:.2}", camera.distance()));
            ui.separator();
            morph_readout(ui, session);
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            view_tools(ui, session);
            let available = ui.available_size();
            if session.stage_open {
                let rect = ui.max_rect();
                ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
            } else {
                output.viewport_pixels = physical_viewport(available, ui.pixels_per_point());
                show_viewport(ui, camera, session, texture_id, available);
            }
        });

    if session.info_open {
        egui::Window::new("モデル情報")
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .show(ui, |ui| {
                show_model_info(ui, session);
            });
    }
    let mut stage_open = session.stage_open;
    let mut stage_fullscreen = session.stage_fullscreen;
    let mut stage = egui::Window::new("舞台")
        .open(&mut stage_open)
        .default_size([960.0, 540.0])
        .resizable(true);
    if stage_fullscreen {
        stage = stage.fixed_rect(ui.ctx().viewport_rect());
    }
    stage.show(ui, |ui| {
        if ui
            .button(if stage_fullscreen {
                "全画面解除"
            } else {
                "全画面"
            })
            .clicked()
        {
            stage_fullscreen = !stage_fullscreen;
        }
        let available = ui.available_size();
        let available = if available.x.is_finite() && available.y.is_finite() {
            available.max(egui::vec2(640.0, 360.0))
        } else {
            egui::vec2(960.0, 540.0)
        };
        output.viewport_pixels = physical_viewport(available, ui.pixels_per_point());
        show_viewport(ui, camera, session, texture_id, available);
    });
    if !stage_open {
        stage_fullscreen = false;
    }
    session.stage_open = stage_open;
    session.stage_fullscreen = stage_fullscreen;

    output
}

fn view_tools(ui: &mut egui::Ui, session: &mut Session) {
    ui.horizontal(|ui| {
        egui::Frame::new()
            .fill(Color32::from_rgb(24, 28, 34))
            .inner_margin(egui::Margin::symmetric(4, 0))
            .show(ui, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(6.0, 0.0);
                ui.spacing_mut().interact_size.y = 16.0;
                ui.spacing_mut().item_spacing.x = 4.0;
                let stage = if session.stage_open {
                    "舞台 ✓"
                } else {
                    "舞台"
                };
                if ui.button(stage).clicked() {
                    session.stage_open = !session.stage_open;
                }
                let info = if session.show_stats {
                    "情報 ✓"
                } else {
                    "情報"
                };
                if ui.button(info).clicked() {
                    session.show_stats = !session.show_stats;
                }
                let guides = if session.show_guides {
                    "座標軸 ✓"
                } else {
                    "座標軸"
                };
                if ui.button(guides).clicked() {
                    session.show_guides = !session.show_guides;
                }
            });
    });
}

fn show_viewport(
    ui: &mut egui::Ui,
    camera: &mut EditorCamera,
    session: &Session,
    texture_id: Option<egui::TextureId>,
    available: egui::Vec2,
) {
    if let Some(texture_id) = texture_id {
        let response = ui.add(
            egui::Image::from_texture(egui::load::SizedTexture::new(texture_id, available))
                .sense(egui::Sense::drag())
                .fit_to_exact_size(available)
                .maintain_aspect_ratio(false),
        );
        if !session.follow_camera {
            apply_viewport_pointer(ui, &response, camera, available.y);
        }
        if session.show_stats {
            let seconds = f64::from(session.frame) / 30.0;
            let total = seconds.max(0.0) as u64;
            let hours = total / 3600;
            let minutes = (total % 3600) / 60;
            let secs = total % 60;
            let text = format!(
                "{}frame ({hours:02}:{minutes:02}:{secs:02})\n{:.0}fps",
                session.frame,
                session.frames_per_second()
            );
            ui.painter().text(
                response.rect.left_top() + egui::vec2(12.0, 10.0),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::proportional(18.0),
                Color32::from_rgb(40, 90, 220),
            );
        }
    } else {
        ui.centered_and_justified(|ui| {
            ui.label("ビューポートの準備中です");
        });
    }
}

fn show_model_info(ui: &mut egui::Ui, session: &mut Session) {
    let Some(card) = session.model_card() else {
        ui.label("モデル未読み込み");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("OK").clicked() {
                session.info_open = false;
            }
        });
        return;
    };
    ui.label(format!("モデル名: {}", card.name));
    let english = if card.english_name.is_empty() {
        "なし".to_string()
    } else {
        card.english_name
    };
    ui.label(format!("英語名: {english}"));
    ui.label(format!("ファイル: {}", card.path));
    ui.label(format!(
        "頂点 {}  面 {}  材質 {}",
        card.vertices, card.faces, card.materials
    ));
    ui.label(format!(
        "ボーン {}  モーフ {}  剛体 {}  ジョイント {}",
        card.bones, card.morphs, card.rigid_bodies, card.joints
    ));
    ui.separator();
    ui.label("コメント");
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .show(ui, |ui| {
            let comment = if card.comment.is_empty() {
                "なし"
            } else {
                card.comment.as_str()
            };
            ui.label(comment);
        });
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("OK").clicked() {
            session.info_open = false;
        }
    });
}

fn playback(ui: &mut egui::Ui, session: &mut Session) {
    ui.horizontal(|ui| {
        if ui.button("再生").clicked() {
            session.play();
        }
        if ui.button("一時停止").clicked() {
            session.pause();
        }
        if ui.button("停止").clicked() {
            session.stop();
        }
    });
    let state = if session.playing {
        "再生中"
    } else {
        "停止中"
    };
    ui.label(format!(
        "フレーム {} / {}  {state}",
        session.frame,
        session.last_frame()
    ));
}

fn model_summary(ui: &mut egui::Ui, session: &mut Session) {
    let names = session.actor_names();
    if names.is_empty() {
        ui.label("モデル未読み込み");
        return;
    }
    ui.label("モデル（クリックでモーションの適用先）");
    let selected = session.selected();
    for (index, name) in names.iter().enumerate() {
        let label = if index == selected {
            format!("● {name}")
        } else {
            format!("○ {name}")
        };
        if ui.button(label).clicked() {
            session.select(index);
        }
    }
    let audio = session.audio_label();
    if audio.is_empty() {
        ui.label("音声なし");
    } else {
        ui.label(format!("音声: {audio}"));
    }
    let Some((name, vertices, faces, bones, materials)) = session.model().map(|model| {
        let name = if model.name.is_empty() {
            "名称なし".to_string()
        } else {
            model.name.clone()
        };
        (
            name,
            model.vertices.len(),
            model.indices.len() / 3,
            model.bones.len(),
            model.materials.len(),
        )
    }) else {
        return;
    };
    ui.label(format!("モデル: {name}"));
    ui.label(format!(
        "頂点 {vertices}  面 {faces}  ボーン {bones}  材質 {materials}"
    ));
    let motion_bones = session
        .motion()
        .map(|motion| motion.bones.len())
        .unwrap_or(0);
    ui.label(format!(
        "一致したボーントラック {} / {motion_bones}",
        session.matched_bones()
    ));
    if session.pose().is_some_and(|pose| pose.orthographic_camera) {
        ui.colored_label(
            Color32::from_rgb(214, 168, 92),
            "平行投影のカメラは未対応です",
        );
    }
}

fn morph_readout(ui: &mut egui::Ui, session: &Session) {
    let rows = session.active_morphs();
    ui.label("モーフ");
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .show(ui, |ui| {
            if rows.is_empty() {
                ui.label("変化しているモーフはありません");
            }
            for (name, weight) in &rows {
                ui.label(format!("{name}  {weight:.2}"));
            }
        });
}

fn apply_viewport_pointer(
    ui: &egui::Ui,
    response: &egui::Response,
    camera: &mut EditorCamera,
    viewport_height_px: f32,
) {
    if response.dragged_by(egui::PointerButton::Secondary) {
        let delta = response.drag_delta();
        apply_camera_input(
            camera,
            CameraInput::Drag(CameraDrag {
                button: CameraPointerButton::Secondary,
                delta_x_px: delta.x,
                delta_y_px: delta.y,
            }),
            viewport_height_px,
        );
    } else if response.dragged_by(egui::PointerButton::Middle) {
        let delta = response.drag_delta();
        apply_camera_input(
            camera,
            CameraInput::Drag(CameraDrag {
                button: CameraPointerButton::Middle,
                delta_x_px: delta.x,
                delta_y_px: delta.y,
            }),
            viewport_height_px,
        );
    }

    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll != 0.0 {
            apply_camera_input(
                camera,
                CameraInput::Scroll { delta_y_px: scroll },
                viewport_height_px,
            );
        }
    }

    let reset = ui.input(|input| input.key_pressed(egui::Key::Home))
        && (response.hovered() || response.has_focus());
    if reset {
        apply_camera_input(camera, CameraInput::Reset, viewport_height_px);
    }
}

fn apply_camera_input(camera: &mut EditorCamera, input: CameraInput, viewport_height_px: f32) {
    if let Err(error) = camera.apply_input(input, viewport_height_px) {
        tracing::warn!(error = %error, "camera input was rejected");
    }
}

fn physical_viewport(available: egui::Vec2, pixels_per_point: f32) -> Option<(u32, u32)> {
    let width = physical_pixels(available.x, pixels_per_point)?;
    let height = physical_pixels(available.y, pixels_per_point)?;
    Some((width, height))
}

fn physical_pixels(logical: f32, pixels_per_point: f32) -> Option<u32> {
    let value = logical * pixels_per_point;
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded < 1.0 || rounded > u32::MAX as f32 {
        return None;
    }
    Some(rounded as u32)
}
