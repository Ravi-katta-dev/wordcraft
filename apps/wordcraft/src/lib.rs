//! Android shared-library entry point for WordCraft.

include!("main.rs");

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    if let Err(error) = run_app(Some(app)) {
        log::error!("WordCraft failed to start on Android: {error}");
    }
}
