//! Builds the native host (`native/host.cpp`, and the Media Kit parts in `native/audio.cpp` and
//! `native/media.cpp`) against the Be API when the target is Haiku (air/OS is a Haiku
//! distribution); elsewhere the crate is only the platform-neutral rasteriser and key tables,
//! and its binary says it needs Haiku.

fn main() {
    for f in ["native/host.cpp", "native/audio.cpp", "native/media.cpp"] {
        println!("cargo:rerun-if-changed={f}");
    }
    // `--version` names the commit it was built from (airos-ci sets it).
    println!("cargo:rerun-if-env-changed=EFFECTCRAFT_BUILD_SHA");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("haiku") {
        return;
    }
    cc::Build::new()
        .cpp(true)
        .file("native/host.cpp")
        .file("native/audio.cpp")
        .file("native/media.cpp")
        .flag("-Wno-multichar")
        .flag("-Wno-unused-parameter")
        .opt_level(2)
        .compile("effectcraft_haiku_host");
    // game: BDirectWindow, which lets frames go straight into the screen, and set_mouse_position.
    for lib in ["be", "game", "media", "tracker", "translation", "stdc++"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
