// The icon is drawn by the same code the app uses, so the executable's icon matches.
#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

fn main() {
    // The widgets' own style follows the system's light or dark mode, which the window
    // reads to pick its theme when set to Auto (see `Theme` in ui/app.slint).
    // Debug builds (and so `cargo test`) keep the element names the UI tests look rows up by;
    // a release build keeps them only when asked, for the README's showcase.
    println!("cargo:rerun-if-env-changed=FEATHERPLAY_UI_DEBUG_INFO");
    let debug =
        std::env::var("PROFILE").as_deref() == Ok("debug") || std::env::var_os("FEATHERPLAY_UI_DEBUG_INFO").is_some();
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into()).with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile the UI");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

/// Gives the Windows executable the app's icon, so Explorer, shortcuts and the taskbar
/// show it before the app has even started, and the version details Explorer shows
/// under Properties.
fn embed_windows_icon() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ico = out.join("featherplay.ico");
    std::fs::write(&ico, ico_file(&[16, 24, 32, 48, 64, 128])).expect("write the icon");
    let rc = out.join("featherplay.rc");
    let icon = format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"));
    std::fs::write(&rc, icon + &version_info()).expect("write the resource script");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("embed the icon");
    println!("cargo:rerun-if-changed=src/icon.rs");
}

/// The VERSIONINFO resource: what the program is, who makes it and which version.
fn version_info() -> String {
    let env = |key: &str| std::env::var(key).unwrap_or_default();
    let version = env("CARGO_PKG_VERSION");
    let numbers: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let numeric = format!("{},0", numbers.join(","));
    let repository = env("CARGO_PKG_REPOSITORY");
    format!(
        r#"1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Featherplay contributors"
      VALUE "FileDescription", "Featherplay, a light audio player"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "featherplay"
      VALUE "LegalCopyright", "Copyright (C) 2026 Featherplay contributors. GPL-3.0-or-later, {repository}"
      VALUE "OriginalFilename", "featherplay.exe"
      VALUE "ProductName", "Featherplay"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}

/// An .ico holding the icon at each size, as 32-bit bitmaps with alpha.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bitmap(size)).collect();
    let mut file = Vec::new();
    file.extend_from_slice(&[0, 0, 1, 0]);
    file.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        let side = if size >= 256 { 0 } else { size as u8 };
        file.extend_from_slice(&[side, side, 0, 0]);
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&(image.len() as u32).to_le_bytes());
        file.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        file.extend_from_slice(&image);
    }
    file
}

/// One icon image: a bitmap header, the pixels bottom row first in BGRA, then an empty
/// one-bit mask (the alpha channel does the masking).
fn bitmap(size: u32) -> Vec<u8> {
    let rgba = icon::rgba(size);
    let mut image = Vec::new();
    for value in [40u32, size, size * 2] {
        image.extend_from_slice(&value.to_le_bytes());
    }
    image.extend_from_slice(&1u16.to_le_bytes());
    image.extend_from_slice(&32u16.to_le_bytes());
    image.extend_from_slice(&[0u8; 24]);
    for row in (0..size).rev() {
        for pixel in rgba[(row * size * 4) as usize..((row + 1) * size * 4) as usize].as_chunks::<4>().0 {
            image.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    let mask_row = (size.div_ceil(32) * 4) as usize;
    image.resize(image.len() + mask_row * size as usize, 0);
    image
}
