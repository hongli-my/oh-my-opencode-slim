use std::sync::mpsc::Receiver;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use eframe::egui;

use crate::gifs::{AnimationFrame, Gifs};
use crate::niri;
use crate::screen::primary_size;
use crate::state::{
    read_state, start_watcher, write_project_window_position, CompanionConfigState, SessionInfo,
    WindowPositionState,
};

const DEFAULT_SIZE: f32 = 120.0;
const GAP: f32 = 10.0;

const SIZE_PRESETS: &[(&str, f32)] = &[("S", 80.0), ("M", 120.0), ("L", 160.0), ("XL", 200.0)];

const MENU_W: f32 = 76.0;
const MENU_H: f32 = 58.0;
const MENU_PAD: f32 = 2.0;
const SURFACE_INSET: f32 = 1.0;

const SIZE_KEY: &str = "companion_size";
const MENU_OPEN_KEY: &str = "companion_menu_open";
const MENU_POS_KEY: &str = "companion_menu_pos";
const MENU_JUST_OPENED_KEY: &str = "companion_menu_just_opened";

/// Agent thumbnail edge as a fraction of the configured size preset. Slightly
/// below 1.0 so a multi-row list stays compact, but large enough that the agent
/// animation reads clearly rather than looking like a narrow strip.
const THUMB_RATIO: f32 = 0.9;
/// Per-row label strip height as a fraction of the thumbnail edge. The project
/// name sits in this strip, top-left above the first icon.
const LABEL_RATIO: f32 = 0.28;
/// Agents rendered per session row before the rest collapse into a "+N" hint.
const MAX_AGENTS_PER_ROW: usize = 5;
/// Sessions rendered before the list is truncated (kept bounded so the window
/// never grows taller than the screen with many concurrent projects).
const MAX_ROWS: usize = 8;
const ROW_SEPARATOR_ALPHA: u8 = 40;

/// Single persisted key for the aggregate window position. The list window is
/// global, so — unlike the old per-project windows — there is one saved
/// position rather than one per cwd.
const GLOBAL_POSITION_KEY: &str = "__companion_window__";

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowGeometryKey {
    position: String,
    custom_x: Option<i32>,
    custom_y: Option<i32>,
    size_px: u32,
    win_w: u32,
    win_h: u32,
    screen_w: u32,
    screen_h: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigKey {
    position: String,
    size: String,
    gif_pack: String,
    loop_style: String,
    speed_bits: u32,
}

fn size_from_config(size: &str) -> f32 {
    match size {
        "small" => 80.0,
        "medium" => 120.0,
        "large" => 160.0,
        "xl" | "xlarge" => 200.0,
        _ => DEFAULT_SIZE,
    }
}

fn config_key(config: Option<&CompanionConfigState>) -> Option<ConfigKey> {
    config.map(|cfg| ConfigKey {
        position: cfg.position.clone(),
        size: cfg.size.clone(),
        gif_pack: normalized_gif_pack(&cfg.gif_pack).to_string(),
        loop_style: normalized_loop_style(&cfg.loop_style).to_string(),
        speed_bits: normalized_speed(cfg.speed).to_bits(),
    })
}

/// Config that drives window placement and animation for the aggregate window.
/// Prefers the top-level `config` block, falling back to the first session that
/// carries one so an older plugin that only writes per-session config still
/// works.
fn config_global<'a>(
    sessions: &'a [SessionInfo],
    global_config: Option<&'a CompanionConfigState>,
) -> Option<&'a CompanionConfigState> {
    global_config.or_else(|| sessions.iter().find_map(|session| session.config.as_ref()))
}

fn normalized_gif_pack(pack: &str) -> &str {
    match pack {
        "default" => "default",
        _ => "default",
    }
}

fn normalized_loop_style(style: &str) -> &str {
    match style {
        "smooth" => "smooth",
        _ => "classic",
    }
}

fn normalized_speed(speed: f32) -> f32 {
    crate::gifs::normalized_speed(speed)
}

fn apply_config(
    key: Option<&ConfigKey>,
    position: &mut String,
    size: &mut f32,
    gif_pack: &mut String,
    loop_style: &mut String,
    speed: &mut f32,
) {
    if let Some(cfg) = key {
        *position = cfg.position.clone();
        *size = size_from_config(&cfg.size);
        *gif_pack = cfg.gif_pack.clone();
        *loop_style = cfg.loop_style.clone();
        *speed = f32::from_bits(cfg.speed_bits);
    } else {
        *position = "bottom-right".to_string();
        *size = DEFAULT_SIZE;
        *gif_pack = "default".to_string();
        *loop_style = "classic".to_string();
        *speed = normalized_speed(f32::NAN);
    }
}

pub(crate) fn place_window(position: &str, screen: [f32; 2], win: [f32; 2]) -> [f32; 2] {
    let (screen_w, screen_h) = (screen[0], screen[1]);
    let (win_w, win_h) = (win[0], win[1]);
    let (x, y) = match position {
        "bottom-left" => (GAP, screen_h - win_h - GAP),
        "top-right" => (screen_w - win_w - GAP, GAP),
        "top-left" => (GAP, GAP),
        _ => (screen_w - win_w - GAP, screen_h - win_h - GAP),
    };
    let x_max = (screen_w - win_w - GAP).max(GAP);
    let y_max = (screen_h - win_h - GAP).max(GAP);
    [x.clamp(GAP, x_max), y.clamp(GAP, y_max)]
}

fn clamp_window_position(pos: [f32; 2], screen: [f32; 2], win: [f32; 2]) -> [f32; 2] {
    let x_max = (screen[0] - win[0] - GAP).max(GAP);
    let y_max = (screen[1] - win[1] - GAP).max(GAP);
    [pos[0].clamp(GAP, x_max), pos[1].clamp(GAP, y_max)]
}

fn restore_window_position(pos: [f32; 2], screen: [f32; 2], win: [f32; 2]) -> [f32; 2] {
    // egui 0.29 exposes monitor size but not monitor origin. If a saved native
    // position is outside origin-zero bounds, it may be on a secondary monitor
    // with a positive or negative origin. Preserve it instead of snapping it
    // back to the primary monitor.
    if 0.0 <= pos[0] && pos[0] < screen[0] && 0.0 <= pos[1] && pos[1] < screen[1] {
        clamp_window_position(pos, screen, win)
    } else {
        pos
    }
}

fn project_name(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Sessions in a stable render order (by cwd, then id), truncated to MAX_ROWS.
/// Stable ordering keeps rows from jumping as sessions go busy/idle.
fn ordered_sessions(sessions: &[SessionInfo]) -> Vec<SessionInfo> {
    let mut ordered: Vec<SessionInfo> = sessions.to_vec();
    ordered.sort_by(|a, b| {
        a.cwd
            .cmp(&b.cwd)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    ordered.truncate(MAX_ROWS);
    ordered
}

/// Agents to draw for a row, capped so a wide fan-out doesn't stretch the row.
fn agents_shown(agent_count: usize) -> usize {
    agent_count.max(1).min(MAX_AGENTS_PER_ROW)
}

/// Layout metrics for a list of `rows` sessions where the widest row shows
/// `max_agents` agents at the given size preset. Each row stacks a label strip
/// (project name, top-left) above a row of square agent thumbnails, so width is
/// driven purely by the agent count — no reserved label column that would leave
/// a black gutter on the right.
struct ListLayout {
    win_w: f32,
    win_h: f32,
    thumb: f32,
    label_h: f32,
    row_h: f32,
}

fn list_layout(rows: usize, max_agents: usize, size: f32) -> ListLayout {
    let thumb = (size * THUMB_RATIO).round().max(24.0);
    let label_h = (thumb * LABEL_RATIO).round().clamp(12.0, 22.0);
    let row_h = thumb + label_h;
    let win_w = thumb * max_agents.max(1) as f32;
    let win_h = row_h * rows.max(1) as f32;
    ListLayout {
        win_w,
        win_h,
        thumb,
        label_h,
        row_h,
    }
}

pub struct CompanionApp {
    state_path: std::path::PathBuf,
    sessions: Vec<SessionInfo>,
    gifs: Gifs,
    rx: Receiver<()>,
    registered: bool,
    size: f32,
    gif_pack: String,
    loop_style: String,
    speed: f32,
    screen: [f32; 2],
    position: String,
    has_modern_config: bool,
    applied_config: Option<ConfigKey>,
    applied_geometry: Option<WindowGeometryKey>,
    last_logged_signature: Option<String>,
    saved_position: Option<WindowPositionState>,
    dragging: bool,
    niri_generation: Arc<AtomicU64>,
}

impl CompanionApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let state_path = crate::state::state_file_path();
        let state = read_state(&state_path);
        crate::log::debug(format!("app new initial_sessions={}", state.sessions.len()));
        let sessions = state.sessions;
        let saved_position = state.window_positions.get(GLOBAL_POSITION_KEY).copied();

        let mut initial_size = DEFAULT_SIZE;
        let mut position = "bottom-right".to_string();
        let mut gif_pack = "default".to_string();
        let mut loop_style = "classic".to_string();
        let mut speed = normalized_speed(f32::NAN);
        let has_modern_config = state.config.is_some();
        let applied_config = config_key(config_global(&sessions, state.config.as_ref()));
        apply_config(
            applied_config.as_ref(),
            &mut position,
            &mut initial_size,
            &mut gif_pack,
            &mut loop_style,
            &mut speed,
        );

        let rx = start_watcher(state_path.clone());

        Self {
            state_path,
            sessions,
            gifs: Gifs::new(),
            rx,
            registered: false,
            size: initial_size,
            gif_pack,
            loop_style,
            speed,
            screen: primary_size(),
            position,
            has_modern_config,
            applied_config,
            applied_geometry: None,
            last_logged_signature: None,
            saved_position,
            dragging: false,
            niri_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn poll(&mut self) -> bool {
        if self.rx.try_recv().is_ok() {
            while self.rx.try_recv().is_ok() {}
            let state = read_state(&self.state_path);
            self.sessions = state.sessions;
            let owned_config = config_global(&self.sessions, state.config.as_ref());
            crate::log::debug(format!(
                "state update sessions={} global_config={:?}",
                self.sessions.len(),
                owned_config
            ));
            self.saved_position = state.window_positions.get(GLOBAL_POSITION_KEY).copied();
            self.has_modern_config = state.config.is_some();
            let next_config = config_key(owned_config);
            let config_changed = self.applied_config != next_config;
            if config_changed {
                apply_config(
                    next_config.as_ref(),
                    &mut self.position,
                    &mut self.size,
                    &mut self.gif_pack,
                    &mut self.loop_style,
                    &mut self.speed,
                );
                self.applied_config = next_config;
            }
            return config_changed;
        }

        let has_modern = self.has_modern_config;
        self.sessions
            .retain(|s| s.pid.map(is_pid_alive).unwrap_or(!has_modern));
        false
    }

    fn update_screen_from_ctx(&mut self, ctx: &egui::Context) {
        if let Some(size) = ctx.input(|i| i.viewport().monitor_size) {
            if 1.0 < size.x && 1.0 < size.y {
                self.screen = [size.x, size.y];
            }
        }
    }
}

impl eframe::App for CompanionApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let config_changed = self.poll();
        self.update_screen_from_ctx(ctx);

        let quit = ctx.data(|d| {
            d.get_temp::<bool>(egui::Id::new("companion_quit"))
                .unwrap_or(false)
        });
        if quit || (self.registered && self.sessions.is_empty()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if !self.registered {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(SIZE_KEY), self.size));
            self.registered = true;
        } else if config_changed {
            // Config/state changes are the source of truth. A right-click picker
            // selection remains local until the config tuple changes.
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(SIZE_KEY), self.size));
        }

        self.size = ctx.data(|d| d.get_temp(egui::Id::new(SIZE_KEY)).unwrap_or(self.size));

        if self.sessions.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("No active sessions");
                    });
                });
            ctx.request_repaint_after(Duration::from_millis(150));
            return;
        }

        let rows = ordered_sessions(&self.sessions);
        let max_agents = rows
            .iter()
            .map(|s| agents_shown(s.active_agents.len()))
            .max()
            .unwrap_or(1);
        let layout = list_layout(rows.len(), max_agents, self.size);
        let win_w = layout.win_w;
        let win_h = layout.win_h;

        // Content signature drives repaint logging only. It must NOT feed the
        // geometry key: status/agent changes leave the window size and position
        // untouched, so folding them into geometry would re-issue InnerSize +
        // OuterPosition viewport commands on every agent tick and make the
        // window flicker.
        let signature = rows
            .iter()
            .map(|s| {
                format!(
                    "{}:{}:{}:{}",
                    s.session_id,
                    s.status,
                    agents_shown(s.active_agents.len()),
                    s.active_agents.join("+"),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        if self.last_logged_signature.as_ref() != Some(&signature) {
            crate::log::debug(format!(
                "render rows={} max_agents={} signature={}",
                rows.len(),
                max_agents,
                signature
            ));
            self.last_logged_signature = Some(signature);
        }

        let saved_position = self.saved_position;
        let menu_open = ctx.data(|d| {
            d.get_temp::<bool>(egui::Id::new(MENU_OPEN_KEY))
                .unwrap_or(false)
        });

        let geometry = WindowGeometryKey {
            position: self.position.clone(),
            custom_x: saved_position.map(|pos| pos.x.round() as i32),
            custom_y: saved_position.map(|pos| pos.y.round() as i32),
            size_px: self.size.round() as u32,
            win_w: win_w.round() as u32,
            win_h: win_h.round() as u32,
            screen_w: self.screen[0].round() as u32,
            screen_h: self.screen[1].round() as u32,
        };
        if self.applied_geometry.as_ref() != Some(&geometry) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(win_w, win_h)));
            let pos = saved_position
                .map(|pos| restore_window_position([pos.x, pos.y], self.screen, [win_w, win_h]))
                .unwrap_or_else(|| place_window(&self.position, self.screen, [win_w, win_h]));
            crate::log::debug(format!(
                "geometry saved_position={:?} pos={:?} win=({}, {}) screen={:?}",
                saved_position, pos, win_w, win_h, self.screen
            ));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                pos[0], pos[1],
            )));
            self.applied_geometry = Some(geometry);
            self.spawn_niri_fallback([win_w, win_h], saved_position);
        }

        if !menu_open && ctx.input(|i| i.pointer.primary_pressed()) {
            self.dragging = true;
        }
        if self.dragging && ctx.input(|i| i.pointer.primary_down()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if ctx.input(|i| i.pointer.primary_released()) {
            if self.dragging {
                self.dragging = false;
                if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                    let position = WindowPositionState {
                        x: rect.min.x,
                        y: rect.min.y,
                    };
                    if write_project_window_position(
                        &self.state_path,
                        GLOBAL_POSITION_KEY,
                        position,
                    )
                    .is_ok()
                    {
                        self.saved_position = Some(position);
                        self.applied_geometry = None;
                    }
                }
            }
        }

        if ctx.input(|i| i.pointer.secondary_released()) {
            let cursor = ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default();
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new(MENU_POS_KEY), [cursor.x, cursor.y]);
                d.insert_temp(egui::Id::new(MENU_OPEN_KEY), true);
                d.insert_temp(egui::Id::new(MENU_JUST_OPENED_KEY), true);
            });
        }

        let time_seconds = ctx.input(|input| input.time);
        // Frames per row, aligned with `rows`. Collected up front so the
        // painting closure below borrows nothing mutable.
        let row_frames: Vec<Vec<AnimationFrame>> = rows
            .iter()
            .map(|session| {
                let count = agents_shown(session.active_agents.len());
                let agents: Vec<&str> = if session.active_agents.is_empty() {
                    vec!["intro"]
                } else {
                    session
                        .active_agents
                        .iter()
                        .take(count)
                        .map(String::as_str)
                        .collect()
                };
                agents
                    .into_iter()
                    .filter_map(|agent| {
                        self.gifs.frame(
                            ctx,
                            agent,
                            &self.gif_pack,
                            self.speed,
                            &self.loop_style,
                            time_seconds,
                        )
                    })
                    .collect()
            })
            .collect();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::ZERO),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                let painter = ui.painter().clone();
                // Fill the actual available rect rather than the pre-resize
                // win_w/win_h: during a resize the two differ for a frame, and
                // painting the stale size leaves a transparent seam that reads
                // as a flicker.
                let surface = ui.max_rect();
                painter.rect_filled(surface, 0.0, egui::Color32::BLACK);

                for (i, session) in rows.iter().enumerate() {
                    let row_top = i as f32 * layout.row_h;
                    render_row(
                        &painter,
                        ctx,
                        session,
                        &row_frames[i],
                        session.active_agents.len(),
                        layout.thumb,
                        layout.label_h,
                        row_top,
                        win_w,
                    );
                    if i > 0 {
                        painter.hline(
                            0.0..=win_w,
                            row_top,
                            egui::Stroke::new(
                                1.0,
                                egui::Color32::from_white_alpha(ROW_SEPARATOR_ALPHA),
                            ),
                        );
                    }
                }
            });

        render_size_picker(ctx, win_w, win_h);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

impl CompanionApp {
    fn spawn_niri_fallback(&self, win_size: [f32; 2], saved_position: Option<WindowPositionState>) {
        let socket = match std::env::var("NIRI_SOCKET") {
            Ok(socket) if !socket.is_empty() => socket,
            _ => return,
        };
        let desired = saved_position
            .map(|pos| restore_window_position([pos.x, pos.y], self.screen, win_size))
            .unwrap_or_else(|| place_window(&self.position, self.screen, win_size));
        if !desired[0].is_finite() || !desired[1].is_finite() {
            return;
        }
        let generation = self.niri_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let position = self.position.clone();
        let target_position = saved_position.map(|pos| [pos.x, pos.y]);
        let screen = self.screen;
        let niri_generation = Arc::clone(&self.niri_generation);
        std::thread::spawn(move || {
            niri::retry_move_current_window(
                socket,
                std::process::id(),
                generation,
                niri_generation,
                position,
                target_position,
                screen,
                win_size,
            );
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    painter: &egui::Painter,
    ctx: &egui::Context,
    session: &SessionInfo,
    agent_frames: &[AnimationFrame],
    total_agents: usize,
    thumb: f32,
    label_h: f32,
    row_top: f32,
    win_w: f32,
) {
    // Label strip sits at the top-left of the row, above the icons.
    let overflow = total_agents.saturating_sub(agent_frames.len());
    let mut label = project_name(&session.cwd);
    if overflow > 0 {
        label = format!("{label} +{overflow}");
    }
    let font_size = (label_h * 0.72).clamp(9.0, 14.0);
    let fid = egui::FontId::proportional(font_size);
    let max_text_w = (win_w - 8.0).max(0.0);
    let fitted = fit_text(ctx, &label, &fid, max_text_w);
    painter.text(
        egui::pos2(4.0, row_top + label_h * 0.5),
        egui::Align2::LEFT_CENTER,
        &fitted,
        fid,
        status_color(&session.status),
    );

    // Agent thumbnails row, directly under the label strip.
    let icons_top = row_top + label_h;
    for (i, frame) in agent_frames.iter().enumerate() {
        let cell = egui::Rect::from_min_size(
            egui::pos2(i as f32 * thumb, icons_top),
            egui::vec2(thumb, thumb),
        );
        painter.image(
            frame.texture_id,
            cell.shrink(SURFACE_INSET),
            frame.uv,
            egui::Color32::WHITE,
        );
    }
}

fn status_color(status: &str) -> egui::Color32 {
    match status {
        "waiting-input" => egui::Color32::from_rgb(240, 200, 90),
        "busy" => egui::Color32::from_rgb(120, 200, 255),
        _ => egui::Color32::from_rgb(210, 210, 214),
    }
}

fn render_size_picker(ctx: &egui::Context, win_w: f32, win_h: f32) {
    let open: bool = ctx.data(|d| d.get_temp(egui::Id::new(MENU_OPEN_KEY)).unwrap_or(false));
    if !open {
        return;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(MENU_OPEN_KEY), false));
        return;
    }

    let pos: [f32; 2] = ctx.data(|d| {
        d.get_temp(egui::Id::new(MENU_POS_KEY))
            .unwrap_or([20.0, 20.0])
    });
    let size: f32 = ctx.data(|d| d.get_temp(egui::Id::new(SIZE_KEY)).unwrap_or(DEFAULT_SIZE));
    let x = pos[0].clamp(MENU_PAD, (win_w - MENU_W - MENU_PAD).max(MENU_PAD));
    let y = pos[1].clamp(MENU_PAD, (win_h - MENU_H - MENU_PAD).max(MENU_PAD));

    let response =
        egui::Area::new(egui::Id::new("size_picker"))
            .fixed_pos(egui::pos2(x, y))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(20, 20, 22))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(35)))
                    .inner_margin(egui::Margin::symmetric(4.0, 4.0))
                    .show(ui, |ui| {
                        ui.set_min_width(MENU_W - MENU_PAD * 2.0);
                        ui.spacing_mut().item_spacing = egui::vec2(1.0, 2.0);
                        ui.label(
                            egui::RichText::new("Size")
                                .size(9.0)
                                .color(egui::Color32::from_rgb(165, 165, 170)),
                        );

                        ui.horizontal(|ui| {
                            for (label, preset) in SIZE_PRESETS {
                                let active = (size - preset).abs() < 0.5;
                                let fill = if active {
                                    egui::Color32::from_rgb(58, 72, 102)
                                } else {
                                    egui::Color32::from_rgb(30, 30, 32)
                                };
                                let text = egui::RichText::new(*label).size(11.0).strong().color(
                                    if active {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::from_rgb(200, 200, 204)
                                    },
                                );
                                if ui
                                    .add_sized(
                                        [17.0, 18.0],
                                        egui::Button::new(text)
                                            .fill(fill)
                                            .stroke(egui::Stroke::NONE),
                                    )
                                    .clicked()
                                {
                                    ctx.data_mut(|d| {
                                        d.insert_temp(egui::Id::new(SIZE_KEY), *preset);
                                        d.insert_temp(egui::Id::new(MENU_OPEN_KEY), false);
                                    });
                                }
                            }
                        });

                        ui.add_space(1.0);

                        if ui
                            .add_sized(
                                [MENU_W - MENU_PAD * 2.0, 17.0],
                                egui::Button::new(
                                    egui::RichText::new("Close")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(240, 110, 110)),
                                )
                                .fill(egui::Color32::from_rgb(38, 24, 26))
                                .stroke(egui::Stroke::NONE),
                            )
                            .clicked()
                        {
                            ctx.data_mut(|d| {
                                d.insert_temp(egui::Id::new(MENU_OPEN_KEY), false);
                                d.insert_temp(egui::Id::new("companion_quit"), true);
                            });
                        }
                    });
            });

    let just_opened = ctx.data_mut(|d| {
        let id = egui::Id::new(MENU_JUST_OPENED_KEY);
        let just_opened = d.get_temp::<bool>(id).unwrap_or(false);
        d.insert_temp(id, false);
        just_opened
    });
    if !just_opened && clicked_outside_menu(ctx, response.response.rect) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(MENU_OPEN_KEY), false));
    }
}

fn clicked_outside_menu(ctx: &egui::Context, menu_rect: egui::Rect) -> bool {
    ctx.input(|i| {
        (i.pointer.primary_released() || i.pointer.secondary_released())
            && i.pointer
                .interact_pos()
                .map(|pos| !menu_rect.contains(pos))
                .unwrap_or(false)
    })
}

fn fit_text(ctx: &egui::Context, text: &str, font_id: &egui::FontId, max_width: f32) -> String {
    let measure = |s: &str| -> f32 {
        ctx.fonts(|f| f.layout_no_wrap(s.to_string(), font_id.clone(), egui::Color32::WHITE))
            .rect
            .width()
    };
    if measure(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let budget = (max_width - measure(ellipsis)).max(0.0);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let end = chars[mid - 1].0 + chars[mid - 1].1.len_utf8();
        if measure(&text[..end]) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ellipsis.to_string();
    }
    let end = chars[lo - 1].0 + chars[lo - 1].1.len_utf8();
    format!("{}{ellipsis}", &text[..end])
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{
        agents_shown, apply_config, config_key, list_layout, ordered_sessions, place_window,
        project_name, restore_window_position, size_from_config, ConfigKey, SessionInfo,
        MAX_AGENTS_PER_ROW, MAX_ROWS, GAP,
    };
    use crate::state::CompanionConfigState;

    fn session(id: &str, cwd: &str, status: &str, agents: &[&str]) -> SessionInfo {
        SessionInfo {
            session_id: id.to_string(),
            cwd: cwd.to_string(),
            active_agents: agents.iter().map(|s| s.to_string()).collect(),
            status: status.to_string(),
            pid: Some(1),
            active_agent: None,
            config: None,
        }
    }

    #[test]
    fn agents_shown_clamps_between_one_and_max() {
        assert_eq!(agents_shown(0), 1);
        assert_eq!(agents_shown(3), 3);
        assert_eq!(agents_shown(20), MAX_AGENTS_PER_ROW);
    }

    #[test]
    fn ordered_sessions_sorts_and_truncates() {
        let sessions: Vec<SessionInfo> = (0..MAX_ROWS + 3)
            .rev()
            .map(|i| session(&format!("s{i}"), &format!("/p{i:02}"), "busy", &["fixer"]))
            .collect();
        let ordered = ordered_sessions(&sessions);
        assert_eq!(ordered.len(), MAX_ROWS);
        // Sorted by cwd ascending, so /p00 comes first.
        assert_eq!(ordered[0].cwd, "/p00");
    }

    #[test]
    fn ordered_sessions_is_stable_by_cwd_then_id() {
        let sessions = vec![
            session("b", "/same", "busy", &["fixer"]),
            session("a", "/same", "idle", &["intro"]),
        ];
        let ordered = ordered_sessions(&sessions);
        assert_eq!(ordered[0].session_id, "a");
        assert_eq!(ordered[1].session_id, "b");
    }

    #[test]
    fn list_layout_grows_with_rows_and_agents() {
        let one = list_layout(1, 1, 120.0);
        let three_rows = list_layout(3, 1, 120.0);
        let three_agents = list_layout(1, 3, 120.0);
        assert!(three_rows.win_h > one.win_h, "more rows -> taller");
        assert!(three_agents.win_w > one.win_w, "more agents -> wider");
        // Width is thumbnail edge times agent count, with no reserved label
        // column, so a single-agent row is exactly one thumbnail wide.
        assert!((one.win_w - one.thumb).abs() < 0.01);
        assert!((three_agents.win_w - three_agents.thumb * 3.0).abs() < 0.01);
        // Height is (thumbnail + label strip) times row count.
        assert!((one.win_h - one.row_h).abs() < 0.01);
        assert!((three_rows.win_h - three_rows.row_h * 3.0).abs() < 0.01);
        assert!(one.row_h > one.thumb, "row includes a label strip");
    }

    #[test]
    fn list_layout_scales_with_size_preset() {
        let small = list_layout(2, 2, 80.0);
        let large = list_layout(2, 2, 160.0);
        assert!(large.win_w > small.win_w);
        assert!(large.win_h > small.win_h);
    }

    #[test]
    fn project_name_uses_basename() {
        assert_eq!(project_name("/a/b/my-project"), "my-project");
        assert_eq!(project_name(""), "unknown");
    }

    #[test]
    fn config_size_defaults_and_presets_work() {
        assert_eq!(size_from_config("small"), 80.0);
        assert_eq!(size_from_config("medium"), 120.0);
        assert_eq!(size_from_config("large"), 160.0);
        assert_eq!(size_from_config("xl"), 200.0);
        assert_eq!(size_from_config("unknown"), 120.0);
    }

    #[test]
    fn top_left_is_gap_gap() {
        assert_eq!(
            place_window("top-left", [1440.0, 900.0], [240.0, 240.0]),
            [GAP, GAP]
        );
    }

    #[test]
    fn bottom_right_stays_anchored_when_height_grows() {
        let small = place_window("bottom-right", [1440.0, 900.0], [240.0, 240.0]);
        let tall = place_window("bottom-right", [1440.0, 900.0], [240.0, 480.0]);
        assert!(tall[1] < small[1]);
        assert!((tall[1] + 480.0 + GAP - 900.0).abs() < 0.01);
    }

    #[test]
    fn bottom_right_stays_anchored_when_width_grows() {
        let small = place_window("bottom-right", [1440.0, 900.0], [240.0, 240.0]);
        let wide = place_window("bottom-right", [1440.0, 900.0], [480.0, 240.0]);
        assert!(wide[0] < small[0]);
        assert!((wide[0] + 480.0 + GAP - 1440.0).abs() < 0.01);
    }

    #[test]
    fn oversized_window_uses_best_effort_gap_anchor() {
        assert_eq!(
            place_window("bottom-right", [300.0, 300.0], [500.0, 500.0]),
            [GAP, GAP]
        );
    }

    #[test]
    fn restore_clamps_origin_zero_positions() {
        assert_eq!(
            restore_window_position([1400.0, 850.0], [1440.0, 900.0], [120.0, 120.0]),
            [1310.0, 770.0]
        );
    }

    #[test]
    fn restore_preserves_negative_origin_monitor_positions() {
        assert_eq!(
            restore_window_position([-900.0, 40.0], [1440.0, 900.0], [120.0, 120.0]),
            [-900.0, 40.0]
        );
    }

    #[test]
    fn config_key_tracks_position_size_and_animation() {
        let cfg = CompanionConfigState {
            enabled: true,
            position: "top-left".into(),
            size: "large".into(),
            gif_pack: "default".into(),
            loop_style: "classic".into(),
            speed: 1.0,
        };
        assert_eq!(
            config_key(Some(&cfg)),
            Some(ConfigKey {
                position: "top-left".into(),
                size: "large".into(),
                gif_pack: "default".into(),
                loop_style: "classic".into(),
                speed_bits: 1.0f32.to_bits(),
            })
        );
        assert_eq!(config_key(None), None);
    }

    #[test]
    fn apply_config_updates_all_fields_and_defaults() {
        let mut position = "bottom-right".to_string();
        let mut size = 200.0;
        let mut gif_pack = "default".to_string();
        let mut loop_style = "classic".to_string();
        let mut speed = 1.0;
        let cfg = ConfigKey {
            position: "top-left".into(),
            size: "small".into(),
            gif_pack: "default".into(),
            loop_style: "smooth".into(),
            speed_bits: 2.0f32.to_bits(),
        };

        apply_config(
            Some(&cfg),
            &mut position,
            &mut size,
            &mut gif_pack,
            &mut loop_style,
            &mut speed,
        );
        assert_eq!(position, "top-left");
        assert_eq!(size, 80.0);
        assert_eq!(loop_style, "smooth");
        assert_eq!(speed, 2.0);

        apply_config(
            None,
            &mut position,
            &mut size,
            &mut gif_pack,
            &mut loop_style,
            &mut speed,
        );
        assert_eq!(position, "bottom-right");
        assert_eq!(size, 120.0);
        assert_eq!(loop_style, "classic");
        assert_eq!(speed, 1.0);
    }
}
