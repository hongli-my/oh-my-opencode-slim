#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod gifs;
mod log;
mod niri;
mod screen;
mod singleton;
mod state;

fn main() -> eframe::Result {
    log::debug(format!("start pid={}", std::process::id()));

    // A single companion window aggregates all OpenCode sessions, so only one
    // instance runs globally regardless of how many processes try to spawn it.
    if !singleton::acquire() {
        log::debug(format!("exit duplicate pid={}", std::process::id()));
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("oh-my-opencode-slim-companion")
            .with_app_id("oh-my-opencode-slim-companion")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(false)
            .with_inner_size([120.0, 120.0]),
        // Run as a macOS accessory app: no Dock icon, never steals focus
        // from the terminal when the windows appear.
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
                builder.with_activation_policy(ActivationPolicy::Accessory);
                builder.with_activate_ignoring_other_apps(false);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = builder;
        })),
        ..Default::default()
    };

    let result = eframe::run_native(
        "oh-my-opencode-slim-companion",
        options,
        Box::new(|cc| Ok(Box::new(app::CompanionApp::new(cc)))),
    );

    singleton::release();
    result
}
