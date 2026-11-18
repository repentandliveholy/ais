fn main() {
    // The updater picks the release asset built for exactly this target.
    println!("cargo:rustc-env=AIS_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rerun-if-changed=build.rs");
}
