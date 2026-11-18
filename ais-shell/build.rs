fn main() {
    // The updater picks the release asset built for exactly this target.
    println!(
        "cargo:rustc-env=AIS_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = tauri_winres::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "ais · AI Sessions")
            .set("FileDescription", "ais · AI Sessions")
            .set("OriginalFilename", "ais-gui.exe");
        if let Err(err) = res.compile() {
            panic!("failed to compile Windows resources: {err}");
        }
    }
}
