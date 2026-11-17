fn main() {
    println!("cargo:rerun-if-changed=icons/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // Keep networking and Shell/COM services demand-loaded until their first use.
    // MSVC resolves these imports on their first call; no service is removed.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-lib=delayimp");
        for library in [
            "crypt32.dll",
            "ws2_32.dll",
            "shell32.dll",
            "ole32.dll",
            "oleaut32.dll",
            "combase.dll",
            "api-ms-win-core-winrt-error-l1-1-0.dll",
        ] {
            println!("cargo:rustc-link-arg=/DELAYLOAD:{library}");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for size in [16, 32] {
            prepare_icon_frame(size).expect("prepare the embedded window icon pixels");
        }
    }
    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("icons/app.ico")
        .compile()
        .expect("embed the Loog application icon");
}

fn prepare_icon_frame(size: u32) -> Result<(), Box<dyn std::error::Error>> {
    let ico = std::fs::read("icons/app.ico")?;
    let entry_count = u16::from_le_bytes([ico[4], ico[5]]) as usize;

    for index in 0..entry_count {
        let entry = 6 + index * 16;
        if entry + 16 > ico.len() {
            break;
        }
        let width = if ico[entry] == 0 {
            256
        } else {
            u32::from(ico[entry])
        };
        let height = if ico[entry + 1] == 0 {
            256
        } else {
            u32::from(ico[entry + 1])
        };
        if width != size || height != size {
            continue;
        }

        let image_length = u32::from_le_bytes([
            ico[entry + 8],
            ico[entry + 9],
            ico[entry + 10],
            ico[entry + 11],
        ]) as usize;
        let image_offset = u32::from_le_bytes([
            ico[entry + 12],
            ico[entry + 13],
            ico[entry + 14],
            ico[entry + 15],
        ]) as usize;
        let image_end = image_offset
            .checked_add(image_length)
            .filter(|end| *end <= ico.len())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid ICO frame")
            })?;
        let pixels = image::load_from_memory_with_format(
            &ico[image_offset..image_end],
            image::ImageFormat::Png,
        )?
        .into_rgba8();
        if pixels.dimensions() != (size, size) {
            return Err("ICO frame dimensions do not match its directory entry".into());
        }
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        std::fs::write(
            output.join(format!("app-icon-{size}.rgba")),
            pixels.into_raw(),
        )?;
        return Ok(());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("Missing {size}x{size} PNG frame in icons/app.ico"),
    )
    .into())
}
