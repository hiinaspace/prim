//! `prim ui`: the Prim room window (docs/room/WINDOW.md).
//!
//! Layers: [`client`] (a model of the live room over `room.sock`, used by the window,
//! `prim ctl chat` and the tests), [`chat`] (the chat channel and the app state keys),
//! [`supervise`] (starting the daemon, voice and other clients) and [`app`] (egui).

pub mod chat;
pub mod client;
pub mod supervise;
#[cfg(feature = "gui")]
pub mod icons;
#[cfg(feature = "gui")]
pub mod app;
#[cfg(feature = "gui")]
pub mod inspector;

#[cfg(feature = "gui")]
use std::process::ExitCode;

#[cfg(feature = "gui")]
/// `prim ui`: run the window. Returns when it closes (the daemon and room stay).
pub fn run(_args: Vec<String>) -> anyhow::Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let handle = rt.handle().clone();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Prim")
            .with_app_id("prim-room-window")
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Prim",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, handle)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(ExitCode::SUCCESS)
}
