//! WordCraft desktop app.
//!
//! Usage: `wordcraft [--control <port>] [--sample] [files…]`
//!
//! `--control <port>` (or `WORDCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server:
//! `{"id":1,"key":"…","method":"engine.execute","params":{"command":"text.insert","params":{"text":"Hi"}}}`.
//! Every request needs the key the app writes to `<settings>/control-key.<instance>` (mode 0600,
//! removed on exit). See `docs/control-protocol.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "macos")]
mod apple_events;
mod control_server;
#[cfg_attr(target_os = "android", path = "file_dialogs_android.rs")]
mod file_dialogs;
#[cfg(any(target_os = "windows", test))]
mod graphics;
mod logging;
#[cfg_attr(target_os = "android", path = "print_android.rs")]
mod print;

use wordcraft_engine::Session;
use wordcraft_ui_egui::{
    Services, UiState, WordApp,
    window_geometry::{WindowGeometry, take_rescue},
};
#[cfg(not(target_os = "android"))]
use wordcraft_ui_egui::paste_picture::ClipboardPicture;
#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

#[cfg(target_os = "android")]
type PlatformApp = Option<AndroidApp>;
#[cfg(not(target_os = "android"))]
type PlatformApp = ();

/// The app, the restored window geometry until the first frame has checked it, the control
/// server's key file (removed on exit), the file dialogs the app asked for and, on macOS, the
/// documents opened from Finder.
struct App(
    WordApp,
    Option<WindowGeometry>,
    Option<control_server::KeyFile>,
    file_dialogs::Launcher,
    #[cfg(target_os = "macos")] fmv_macos_events::Inbox,
);

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        apple_events::poll(&self.4, &mut self.0, ctx);
        self.0.logic(ctx);
        self.3.show(frame);
        let prev = self.0.ui.window;
        self.0.ui.window = ctx.input(|i| WindowGeometry::track(prev, i.viewport(), i.viewport_rect().size()));
        if let Some(pos) = ctx.input(|i| take_rescue(&mut self.1, i.viewport())) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.0.close_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.0.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.raw_input_hook(raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.0.ui(ui);
        self.3.show(frame);
    }
    fn on_exit(&mut self) {
        save_prefs(&self.0);
        if let Some(key_file) = &self.2 {
            key_file.remove();
        }
    }
}

fn prefs_path() -> Option<std::path::PathBuf> {
    wordcraft_control_key::settings_dir().map(|b| b.join("ui.json"))
}
fn log_dir() -> Option<std::path::PathBuf> {
    Some(prefs_path()?.parent()?.join("logs"))
}
fn prefs_enabled() -> bool {
    std::env::var_os("WORDCRAFT_NO_PREFS").is_none()
}
fn load_prefs(app: &mut WordApp) {
    if !prefs_enabled() { return; }
    if let Some(p) = prefs_path() && let Ok(bytes) = std::fs::read(&p) && let Ok(ui) = serde_json::from_slice::<UiState>(&bytes) {
        app.apply_prefs(ui);
    }
}
fn saved_window() -> Option<WindowGeometry> {
    if !prefs_enabled() { return None; }
    let bytes = std::fs::read(prefs_path()?).ok()?;
    serde_json::from_slice::<UiState>(&bytes).ok()?.window?.sanitized()
}
fn save_prefs(app: &WordApp) {
    if !prefs_enabled() { return; }
    if let Some(p) = prefs_path() {
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        if let Ok(bytes) = serde_json::to_vec_pretty(&app.prefs()) { let _ = std::fs::write(&p, bytes); }
    }
}
fn services(file_dialog: file_dialogs::Hook) -> Services {
    Services {
        file_dialog: Some(file_dialog),
        print: Some(print::hook()),
        #[cfg(not(target_os = "android"))]
        clipboard_picture: Some(Box::new(clipboard_picture)),
        ..Default::default()
    }
}
#[cfg(not(target_os = "android"))]
fn clipboard_picture() -> Option<ClipboardPicture> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| log::warn!("clipboard: {e}")).ok()?;
    if let Ok(files) = clipboard.get().file_list() && !files.is_empty() { return Some(ClipboardPicture::Files(files)); }
    let image = clipboard.get_image().ok()?;
    Some(ClipboardPicture::Pixels { width: image.width, height: image.height, rgba: image.bytes.into_owned() })
}
fn app_icon() -> Option<egui::IconData> {
    #[cfg(target_os = "macos")]
    let png: &[u8] = include_bytes!("../../../assets/app-icon/wordcraft-macos-512.png");
    #[cfg(not(target_os = "macos"))]
    let png: &[u8] = include_bytes!("../../../assets/app-icon/hicolor/256x256/apps/ai.storyteller.wordcraft.png");
    eframe::icon_data::from_png_bytes(png).map_err(|e| log::warn!("app icon: {e}")).ok()
}
fn build_sha() -> &'static str {
    option_env!("WORDCRAFT_BUILD_SHA").map(|s| s.get(..8).unwrap_or(s)).unwrap_or("dev")
}

pub fn run_app(platform_app: PlatformApp) -> eframe::Result {
    let logger = logging::install();
    logging::install_panic_hook();
    let mut control_port: Option<u16> = std::env::var("WORDCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok());
    let mut files = Vec::new();
    let mut sample = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = args.next().and_then(|p| p.parse().ok()),
            "--sample" => sample = true,
            "--version" => { println!("wordcraft {} ({})", env!("CARGO_PKG_VERSION"), build_sha()); return Ok(()); }
            _ => files.push(a),
        }
    }
    if let Some(logger) = logger {
        match log_dir().filter(|_| prefs_enabled()) {
            Some(dir) => match logger.attach_dir(&dir) {
                Ok(path) => log::info!("WordCraft {} ({}), log file {}", env!("CARGO_PKG_VERSION"), build_sha(), path.display()),
                Err(e) => log::warn!("no log file: {e}"),
            },
            None => logger.no_file(),
        }
    }
    let mut options = eframe::NativeOptions {
        #[cfg(target_os = "android")]
        android_app: platform_app,
        viewport: egui::ViewportBuilder::default().with_title("WordCraft").with_app_id("ai.storyteller.wordcraft").with_inner_size([1440.0, 920.0]).with_min_inner_size([760.0, 480.0]).with_drag_and_drop(true).with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false),
        ..Default::default()
    };
    if let Some(icon) = app_icon() { options.viewport = options.viewport.with_icon(icon); }
    #[cfg(target_os = "windows")]
    graphics::configure(&mut options, eframe::wgpu::Backends::from_env());
    let restored = saved_window();
    if let Some(window) = restored { options.viewport = window.apply(options.viewport); }
    #[cfg(target_os = "macos")]
    let apple_events = apple_events::AppleEvents::install();
    #[cfg(target_os = "macos")]
    let apple_events = &apple_events;
    eframe::run_native("WordCraft", options, Box::new(move |cc| {
        let doc = if sample { wordcraft_engine::sample::sample_document() } else { wordcraft_doc::Document::new() };
        let (file_dialog, dialogs) = file_dialogs::hook(&cc.egui_ctx);
        let mut app = WordApp::new(Session::new(doc), services(file_dialog));
        load_prefs(&mut app);
        app.ui.window = restored;
        app.integrated_titlebar = cfg!(target_os = "macos");
        let mut key_file = None;
        if let Some(port) = control_port && let Some((rx, file)) = control_server::start(port, cc.egui_ctx.clone(), wordcraft_control_key::settings_dir().as_deref()) {
            app = app.with_control(rx);
            key_file = file;
        }
        for f in files { if let Err(e) = app.run("file.open", serde_json::json!({"path": f})) { log::warn!("{f}: {e}"); } }
        Ok(Box::new(App(app, restored, key_file, dialogs, #[cfg(target_os = "macos")] apple_events.connect(&cc.egui_ctx))) as Box<dyn eframe::App>)
    }))
}

#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result { run_app(()) }
#[cfg(target_os = "android")]
fn main() -> eframe::Result { run_app(None) }
