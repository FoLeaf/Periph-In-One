fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    // README screenshots do not affect the embedded application resources.
    for entry in std::fs::read_dir("assets").expect("assets directory is missing") {
        let path = entry.expect("cannot read asset entry").path();
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/app.ico")
            .set("ProductName", "Periph In One")
            .set("FileDescription", "Periph In One")
            .set("OriginalFilename", "periph-in-one.exe")
            .compile()
            .expect("Windows icon resource compilation failed");
        slint_build::compile_with_config(
            "ui/app.slint",
            slint_build::CompilerConfiguration::new().with_style("fluent-light".into()),
        )
        .expect("Slint UI compilation failed");
    }
}
