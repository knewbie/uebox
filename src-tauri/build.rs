fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=native/macos/capture.m");
        cc::Build::new()
            .file("native/macos/capture.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("uebox_macos_capture");
        for framework in ["Foundation", "AVFoundation", "CoreMedia"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        // The native code checks macOS 15 before it makes any recording calls.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }
    tauri_build::build()
}
