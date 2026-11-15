fn main() {
    // The plugin calls functions that libhttrack exports. HTTrack has already
    // loaded libhttrack when it loads the plugin, so macOS resolves them then.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-undefined,dynamic_lookup");
    }
}
