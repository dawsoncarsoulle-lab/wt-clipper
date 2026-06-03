use std::time::Duration;

use chrono::Local;
use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, RichText, Stroke};

use crate::{
    capture::buffer::ClipReason,
    capture::quality::QualityPreset,
    config::AppConfig,
    doctor::{DoctorReport, DoctorStatus},
    ui::{
        bridge::{AppEvent, Bridge, ClipInfo, UiCommand},
        theme::Theme,
    },
};

#[derive(Debug, Clone)]
struct EventEntry {
    timestamp: String,
    reason: ClipReason,
    vehicle: Option<String>,
    target: Option<String>,
    description: String,
}

#[derive(Debug, Clone)]
struct Toast {
    message: String,
    ttl: f32,
    color: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Dashboard,
    Clips,
    Configuration,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipFilter {
    All,
    Kills,
    MultiKills,
    Deaths,
}

pub struct WtClipperApp {
    bridge: Bridge,
    active_screen: Screen,
    wt_connected: bool,
    player_name: Option<String>,
    buffer_filled_secs: f32,
    buffer_total_secs: f32,
    disk_used_bytes: u64,
    session_kills: u32,
    session_multi_kills: u32,
    clips: Vec<ClipInfo>,
    events: Vec<EventEntry>,
    toasts: Vec<Toast>,
    config: AppConfig,
    search: String,
    filter: ClipFilter,
    diagnostics: Option<DoctorReport>,
    diagnostics_running: bool,
    clips_total_bytes: u64,
    last_refresh: f32,
}

impl WtClipperApp {
    pub fn new(cc: &eframe::CreationContext<'_>, bridge: Bridge, config: AppConfig) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        Theme::apply(&cc.egui_ctx);
        let app = Self {
            bridge,
            active_screen: Screen::Dashboard,
            wt_connected: false,
            player_name: config.war_thunder.player_name.clone(),
            buffer_filled_secs: 0.0,
            buffer_total_secs: config.clip.seconds as f32,
            disk_used_bytes: 0,
            session_kills: 0,
            session_multi_kills: 0,
            clips: Vec::new(),
            events: Vec::new(),
            toasts: Vec::new(),
            config,
            search: String::new(),
            filter: ClipFilter::All,
            diagnostics: None,
            diagnostics_running: false,
            clips_total_bytes: 0,
            last_refresh: 0.0,
        };
        let _ = app.bridge.cmd_tx.send(UiCommand::LoadClips);
        let _ = app.bridge.cmd_tx.send(UiCommand::RunDiagnostics);
        app
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.bridge.event_rx.try_recv() {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::WtConnected => self.wt_connected = true,
            AppEvent::WtDisconnected => self.wt_connected = false,
            AppEvent::KillDetected {
                reason,
                vehicle,
                target,
                description,
                ..
            } => {
                if reason == ClipReason::MultiKill {
                    self.session_multi_kills = self.session_multi_kills.saturating_add(1);
                }
                if matches!(reason, ClipReason::TargetDestroyed | ClipReason::MultiKill) {
                    self.session_kills = self.session_kills.saturating_add(1);
                }
                self.events.insert(
                    0,
                    EventEntry {
                        timestamp: Local::now().format("%H:%M:%S").to_string(),
                        reason,
                        vehicle,
                        target,
                        description,
                    },
                );
                self.events.truncate(20);
            }
            AppEvent::ClipSaved {
                path,
                reason,
                duration_seconds,
                size_bytes,
            } => {
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.display().to_string());
                self.clips.insert(
                    0,
                    ClipInfo {
                        path: path.clone(),
                        thumbnail_path: path
                            .with_extension("jpg")
                            .exists()
                            .then(|| path.with_extension("jpg")),
                        preview_url: None,
                        file_name,
                        reason,
                        size_bytes,
                        duration_seconds,
                        modified_secs_ago: 0,
                    },
                );
                self.clips_total_bytes = self.clips_total_bytes.saturating_add(size_bytes);
                self.toasts.push(Toast {
                    message: "Clip sauvegardé".to_owned(),
                    ttl: 4.0,
                    color: Theme::KILL_GREEN,
                });
                self.disk_used_bytes = self.clips_total_bytes;

                let thumb_path = path.with_extension("jpg");
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("ffmpeg")
                        .args(["-y", "-i"])
                        .arg(&path)
                        .args(["-vframes", "1", "-s", "320x180"])
                        .arg(&thumb_path)
                        .output();
                });
            }
            AppEvent::ClipFailed { message } => self.toasts.push(Toast {
                message,
                ttl: 5.0,
                color: Theme::DEATH_RED,
            }),
            AppEvent::BufferProgress {
                filled_secs,
                total_secs,
            } => {
                self.buffer_filled_secs = filled_secs;
                self.buffer_total_secs = total_secs.max(1.0);
            }
            AppEvent::DiskUsage { used_bytes } => self.disk_used_bytes = used_bytes,
            AppEvent::ClipsLoaded { clips, total_bytes } => {
                self.clips = clips.clone();
                self.clips_total_bytes = total_bytes;
                self.disk_used_bytes = total_bytes;

                std::thread::spawn(move || {
                    for clip in clips {
                        let thumb_path = clip.path.with_extension("jpg");
                        if !thumb_path.exists() {
                            let _ = std::process::Command::new("ffmpeg")
                                .args(["-y", "-i"])
                                .arg(&clip.path)
                                .args(["-vframes", "1", "-s", "320x180"])
                                .arg(&thumb_path)
                                .output();
                        }
                    }
                });
            }
            AppEvent::DiagnosticsReady(report) => {
                self.diagnostics = Some(report);
                self.diagnostics_running = false;
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new("WT CLIPPER")
                    .strong()
                    .size(18.0)
                    .color(Theme::ACCENT),
            );
            ui.label(
                RichText::new("combat recorder")
                    .color(Theme::TEXT_MUTED)
                    .size(11.0),
            );
        });
        ui.add_space(20.0);
        if let Some(player_name) = &self.player_name {
            ui.label(
                RichText::new(player_name)
                    .monospace()
                    .color(Theme::TEXT_MUTED),
            );
            ui.add_space(10.0);
        }
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new("🎬 Clip manuel"),
            )
            .clicked()
        {
            let _ = self.bridge.cmd_tx.send(UiCommand::SaveManualClip);
            self.toasts.push(Toast {
                message: "Enregistrement manuel en cours...".to_owned(),
                ttl: 3.0,
                color: Theme::ACCENT,
            });
        }
        ui.add_space(14.0);
        self.nav_button(ui, Screen::Dashboard, "📊 Dashboard");
        ui.add_space(4.0);
        self.nav_button(ui, Screen::Clips, "🎞 Clips");
        ui.add_space(4.0);
        self.nav_button(ui, Screen::Configuration, "⚙ Configuration");
        ui.add_space(4.0);
        self.nav_button(ui, Screen::Diagnostics, "🩺 Diagnostics");
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            status_row(ui, "WT", self.wt_connected, ctx_seconds(ui));
            status_row(ui, "Buffer", self.buffer_filled_secs > 0.0, ctx_seconds(ui));
        });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, screen: Screen, label: &str) {
        let selected = self.active_screen == screen;
        let text = if selected {
            RichText::new(label).color(Theme::TEXT_PRIMARY).strong()
        } else {
            RichText::new(label).color(Theme::TEXT_MUTED)
        };
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new(text).selected(selected),
            )
            .clicked()
        {
            self.active_screen = screen;
            if screen == Screen::Clips {
                let _ = self.bridge.cmd_tx.send(UiCommand::LoadClips);
            }
        }
    }

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dashboard");
        ui.add_space(14.0);

        ui.columns(4, |columns| {
            metric_card(
                &mut columns[0],
                "Kills session",
                self.session_kills.to_string(),
                Theme::KILL_GREEN,
            );
            metric_card(
                &mut columns[1],
                "Multi-kills",
                self.session_multi_kills.to_string(),
                Theme::MULTI_PURPLE,
            );
            metric_card(
                &mut columns[2],
                "Buffer",
                format!("{:.0}s", self.buffer_total_secs),
                Theme::ACCENT,
            );
            metric_card(
                &mut columns[3],
                "Disque",
                human_bytes(self.disk_used_bytes),
                Theme::TEXT_PRIMARY,
            );
        });

        ui.add_space(20.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Replay buffer").strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(format!(
                        "{:.0}/{:.0}s",
                        self.buffer_filled_secs, self.buffer_total_secs
                    ));
                });
            });
            ui.add_space(10.0);
            buffer_bar(ui, self.buffer_filled_secs, self.buffer_total_secs);
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Feed événements").strong());
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(310.0)
                .show(ui, |ui| {
                    if self.events.is_empty() {
                        ui.label(RichText::new("Aucun événement détecté").color(Theme::TEXT_MUTED));
                    }
                    for event in &self.events {
                        ui.horizontal(|ui| {
                            ui.monospace(&event.timestamp);
                            badge(ui, reason_label(event.reason), reason_color(event.reason));
                            if let Some(vehicle) = &event.vehicle {
                                ui.label(RichText::new(vehicle).color(Theme::TEXT_PRIMARY));
                            }
                            if let Some(target) = &event.target {
                                ui.label(RichText::new(target).color(Theme::TEXT_MUTED));
                            }
                            ui.label(RichText::new(&event.description).color(Theme::TEXT_MUTED));
                        });
                    }
                });
        });
    }

    fn clips_screen(&mut self, ui: &mut egui::Ui) {
        let current_time = ctx_seconds(ui);
        if current_time - self.last_refresh > 5.0 {
            let _ = self.bridge.cmd_tx.send(UiCommand::LoadClips);
            self.last_refresh = current_time;
        }

        ui.horizontal(|ui| {
            ui.heading("Clips");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("🔄 Rafraîchir").clicked() {
                    let _ = self.bridge.cmd_tx.send(UiCommand::LoadClips);
                    self.last_refresh = current_time;
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_sized(
                [280.0, 32.0],
                egui::TextEdit::singleline(&mut self.search).hint_text("🔍 Rechercher"),
            );
            for (filter, label) in [
                (ClipFilter::All, "Tous"),
                (ClipFilter::Kills, "Kills"),
                (ClipFilter::MultiKills, "Multi-kills"),
                (ClipFilter::Deaths, "Morts"),
            ] {
                if ui.selectable_label(self.filter == filter, label).clicked() {
                    self.filter = filter;
                }
            }
        });
        ui.add_space(14.0);
        let mut clips: Vec<ClipInfo> = self
            .clips
            .iter()
            .filter(|clip| self.clip_matches(clip))
            .cloned()
            .collect();

        clips.sort_by_key(|c| c.modified_secs_ago);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for clip in &clips {
                clip_row(ui, clip, &self.bridge.cmd_tx);
                ui.add_space(16.0);
            }
            if clips.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Aucun clip trouvé")
                            .color(Theme::TEXT_MUTED)
                            .size(18.0),
                    );
                });
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("{} clips", clips.len()));
            ui.label(RichText::new(human_bytes(self.clips_total_bytes)).color(Theme::TEXT_MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("🗑 Vider tout").clicked() {
                    for clip in &clips {
                        let _ = self
                            .bridge
                            .cmd_tx
                            .send(UiCommand::DeleteClip(clip.path.clone()));
                    }
                    self.clips.clear();
                    self.clips_total_bytes = 0;
                    self.disk_used_bytes = 0;
                }
            });
        });
    }

    fn clip_matches(&self, clip: &ClipInfo) -> bool {
        let search_matches = self.search.trim().is_empty()
            || clip
                .file_name
                .to_ascii_lowercase()
                .contains(&self.search.to_ascii_lowercase());
        let filter_matches = match self.filter {
            ClipFilter::All => true,
            ClipFilter::Kills => clip.reason == ClipReason::TargetDestroyed,
            ClipFilter::MultiKills => clip.reason == ClipReason::MultiKill,
            ClipFilter::Deaths => clip.reason == ClipReason::PlayerDestroyed,
        };
        search_matches && filter_matches
    }

    fn configuration_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Configuration");
        ui.add_space(12.0);
        card(ui, |ui| {
            ui.add(
                egui::Slider::new(&mut self.config.clip.seconds, 15..=120).text("Buffer seconds"),
            );
            ui.add(
                egui::Slider::new(&mut self.config.clip.segment_seconds, 1..=5)
                    .text("Segment seconds"),
            );
            ui.add(
                egui::Slider::new(&mut self.config.clip.post_event_seconds, 0..=15)
                    .text("Post event delay"),
            );
            egui::ComboBox::from_label("Qualité")
                .selected_text(quality_label(self.config.clip.quality))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.config.clip.quality, QualityPreset::Low, "Low");
                    ui.selectable_value(
                        &mut self.config.clip.quality,
                        QualityPreset::Medium,
                        "Medium",
                    );
                    ui.selectable_value(&mut self.config.clip.quality, QualityPreset::High, "High");
                    if ui.selectable_label(false, "Ultra").clicked() {
                        self.config.clip.quality = QualityPreset::High;
                        self.config.clip.video_bitrate_kbps = 30_000;
                    }
                });
            ui.checkbox(&mut self.config.clip.keep_segments, "keep_segments");
            ui.horizontal(|ui| {
                ui.label("output_dir");
                ui.add_sized(
                    [380.0, 28.0],
                    egui::TextEdit::singleline(&mut self.config.clip.output_dir),
                );
                if ui.button("📂 Ouvrir").clicked() {
                    let _ = self.bridge.cmd_tx.send(UiCommand::OpenOutputFolder);
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("💾 Sauvegarder").clicked() {
                    let _ = self
                        .bridge
                        .cmd_tx
                        .send(UiCommand::UpdateConfig(self.config.clone()));
                    self.toasts.push(Toast {
                        message: "Configuration envoyée".to_owned(),
                        ttl: 3.0,
                        color: Theme::ACCENT,
                    });
                }
                if ui.button("🔄 Redémarrer le buffer").clicked() {
                    let _ = self.bridge.cmd_tx.send(UiCommand::RestartBuffer);
                    self.toasts.push(Toast {
                        message: "Buffer redémarré".to_owned(),
                        ttl: 3.0,
                        color: Theme::ACCENT,
                    });
                }
            });
        });
    }

    fn diagnostics_screen(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Diagnostics");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("🔄 Relancer les checks").clicked() {
                    self.diagnostics_running = true;
                    let _ = self.bridge.cmd_tx.send(UiCommand::RunDiagnostics);
                }
            });
        });
        ui.add_space(12.0);
        if self.diagnostics_running {
            ui.spinner();
            return;
        }
        if let Some(report) = &self.diagnostics {
            card(ui, |ui| {
                for check in &report.checks {
                    ui.horizontal_wrapped(|ui| {
                        let (icon, color) = match check.status {
                            DoctorStatus::Ok => ("✅", Theme::KILL_GREEN),
                            DoctorStatus::Warn => ("⚠", Color32::from_rgb(220, 170, 70)),
                            DoctorStatus::Error => ("❌", Theme::DEATH_RED),
                        };
                        ui.label(RichText::new(icon).color(color).size(18.0));
                        ui.label(RichText::new(&check.name).strong());
                        ui.label(RichText::new(&check.message).color(Theme::TEXT_MUTED));
                        if let Some(hint) = &check.hint {
                            ui.label(RichText::new(hint).color(Theme::TEXT_MUTED).italics());
                        }
                    });
                }
                ui.separator();
                ui.label(&report.summary);
            });
        } else {
            ui.label(RichText::new("Checks non lancés").color(Theme::TEXT_MUTED));
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|input| input.unstable_dt).min(0.2);
        for toast in &mut self.toasts {
            toast.ttl -= dt;
        }
        self.toasts.retain(|toast| toast.ttl > 0.0);
        egui::Area::new("toasts".into())
            .anchor(Align2::RIGHT_TOP, [-18.0, 18.0])
            .show(ctx, |ui| {
                for toast in &self.toasts {
                    egui::Frame::default()
                        .fill(Theme::BG_CARD)
                        .stroke(Stroke::new(1.0, toast.color))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&toast.message).color(Theme::TEXT_PRIMARY));
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for WtClipperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::SidePanel::left("sidebar")
            .exact_width(220.0)
            .frame(
                egui::Frame::default()
                    .fill(Theme::BG_SECONDARY)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Theme::BG_PRIMARY))
            .show(ctx, |ui| {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(940.0);
                        match self.active_screen {
                            Screen::Dashboard => self.dashboard(ui),
                            Screen::Clips => self.clips_screen(ui),
                            Screen::Configuration => self.configuration_screen(ui),
                            Screen::Diagnostics => self.diagnostics_screen(ui),
                        }
                    });
                    ui.add_space(20.0);
                });
            });
        self.draw_toasts(ctx);
    }
}

fn metric_card(ui: &mut egui::Ui, label: &str, value: String, color: Color32) {
    egui::Frame::default()
        .fill(Theme::BG_CARD)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(82.0);
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(label).color(Theme::TEXT_MUTED).size(13.0));
                ui.add_space(2.0);
                ui.label(RichText::new(value).color(color).size(28.0).strong());
            });
        });
}

fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(Theme::BG_CARD)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(16))
        .show(ui, add_contents)
        .inner
}

fn buffer_bar(ui: &mut egui::Ui, filled: f32, total: f32) {
    let desired = egui::vec2(ui.available_width(), 24.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let percent = (filled / total.max(1.0)).clamp(0.0, 1.0);
    let segment_count = (rect.width() / 4.0).floor().max(1.0) as usize;
    let active_count = ((segment_count as f32) * percent).round() as usize;
    for index in 0..segment_count {
        let x = rect.left() + index as f32 * 4.0;
        let color = if index <= active_count {
            Theme::ACCENT
        } else {
            Theme::BG_SECONDARY
        };
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(3.0, rect.height())),
            CornerRadius::same(1),
            color,
        );
    }
}

fn status_row(ui: &mut egui::Ui, label: &str, on: bool, seconds: f32) {
    ui.horizontal(|ui| {
        let pulse = ((seconds * 4.0).sin() * 0.5 + 0.5) * 0.55 + 0.45;
        let color = if on {
            Theme::ACCENT.linear_multiply(pulse)
        } else {
            Theme::DEATH_RED
        };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(label).color(Theme::TEXT_MUTED));
    });
}

fn ctx_seconds(ui: &egui::Ui) -> f32 {
    ui.ctx().input(|input| input.time as f32)
}

fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::default()
        .fill(color.linear_multiply(0.22))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.75)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(11.0).strong());
        });
}

fn clip_row(
    ui: &mut egui::Ui,
    clip: &ClipInfo,
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<UiCommand>,
) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            let thumb_path = clip.path.with_extension("jpg");

            // Rendu de l'image (plus grand, 160x90)
            if thumb_path.exists() {
                // Utilisation d'un chemin absolu sécurisé pour le chargeur "file://"
                let abs_path = thumb_path
                    .canonicalize()
                    .unwrap_or_else(|_| thumb_path.clone());
                let uri = format!("file://{}", abs_path.display());

                ui.add(
                    egui::Image::new(uri)
                        .fit_to_exact_size(egui::vec2(160.0, 90.0))
                        .corner_radius(8.0),
                );
            } else {
                let (thumb, _) =
                    ui.allocate_exact_size(egui::vec2(160.0, 90.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(thumb, CornerRadius::same(8), Theme::BG_SECONDARY);
                ui.painter().text(
                    thumb.center(),
                    Align2::CENTER_CENTER,
                    "🎞",
                    FontId::proportional(28.0),
                    Theme::TEXT_MUTED,
                );
            }

            ui.add_space(16.0); // Espace plus aéré

            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(&clip.file_name)
                        .monospace()
                        .size(16.0)
                        .color(Theme::TEXT_PRIMARY),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    badge(ui, reason_label(clip.reason), reason_color(clip.reason));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(human_bytes(clip.size_bytes))
                            .color(Theme::TEXT_MUTED)
                            .size(14.0),
                    );
                    ui.add_space(8.0);

                    let dur_str = if clip.duration_seconds > 0 {
                        format!("{}s", clip.duration_seconds)
                    } else {
                        "??s".to_string()
                    };
                    ui.label(RichText::new(dur_str).color(Theme::TEXT_MUTED).size(14.0));
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new(relative_time(clip.modified_secs_ago))
                            .color(Theme::TEXT_MUTED)
                            .size(14.0),
                    );
                });
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("🗑 Supprimer").clicked() {
                    let _ = cmd_tx.send(UiCommand::DeleteClip(clip.path.clone()));
                    let _ = std::fs::remove_file(&thumb_path);
                    let _ = cmd_tx.send(UiCommand::LoadClips); // Force refresh backend
                }
                ui.add_space(8.0);
                if ui.button("📂 Ouvrir").clicked() {
                    let _ = cmd_tx.send(UiCommand::OpenOutputFolder);
                }
            });
        });
    });
}

fn reason_label(reason: ClipReason) -> &'static str {
    match reason {
        ClipReason::TargetDestroyed => "KILL",
        ClipReason::PlayerDestroyed => "MORT",
        ClipReason::MultiKill => "MULTI",
        ClipReason::Manual => "MANUAL",
        ClipReason::Unknown => "CLIP",
    }
}

fn reason_color(reason: ClipReason) -> Color32 {
    match reason {
        ClipReason::TargetDestroyed | ClipReason::Manual => Theme::KILL_GREEN,
        ClipReason::MultiKill => Theme::MULTI_PURPLE,
        ClipReason::PlayerDestroyed => Theme::DEATH_RED,
        ClipReason::Unknown => Theme::TEXT_MUTED,
    }
}

fn quality_label(quality: QualityPreset) -> &'static str {
    match quality {
        QualityPreset::Low => "Low",
        QualityPreset::Medium => "Medium",
        QualityPreset::High => "High",
    }
}

fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

fn relative_time(seconds: u64) -> String {
    if seconds < 60 {
        "à l'instant".to_owned()
    } else if seconds < 3600 {
        format!("il y a {} min", seconds / 60)
    } else if seconds < 86_400 {
        format!("il y a {} h", seconds / 3600)
    } else {
        format!("il y a {} j", seconds / 86_400)
    }
}
