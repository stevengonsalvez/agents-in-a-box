fn main() {
    // Only the window build embeds the Tauri context; the host library and its
    // tests build without the platform webview.
    #[cfg(feature = "app")]
    tauri_build::build();
}
