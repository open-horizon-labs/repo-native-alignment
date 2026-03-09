fn main() {
    // Pre-compile Metal shaders at build time instead of JIT at runtime.
    // Without this, first GPU inference has a 1-3s cold start while shaders compile.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-env=CANDLE_METAL_FORCE_RELEASE=1");
}
