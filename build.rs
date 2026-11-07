fn main() {
    println!("cargo:rerun-if-changed=icons/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=scripts/terminal_console_probe.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("loog-console-probe.exe");
        let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap())
            .arg("--edition=2024")
            .arg("--target")
            .arg(std::env::var_os("TARGET").unwrap())
            .arg("-Copt-level=s")
            .arg("-Cdebuginfo=0")
            .arg("scripts/terminal_console_probe.rs")
            .arg("-o")
            .arg(output)
            .status()
            .expect("compile the embedded console observer");
        assert!(status.success(), "compile the embedded console observer");
    }

    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("icons/app.ico")
        .compile()
        .expect("embed the Loog application icon");
}
