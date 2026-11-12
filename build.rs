fn main() {
    println!("cargo:rerun-if-changed=icons/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("icons/app.ico")
        .compile()
        .expect("embed the Loog application icon");
}
