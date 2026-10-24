//! Builds the vendored Basis Universal transcoder (Apache-2.0) and its Zstandard decoder (BSD)
//! together with Cyclops's small C++ shim, for KTX2 ETC1S/UASTC textures.

fn main() {
    let tp = "third_party";
    println!("cargo:rerun-if-changed={tp}/basis_shim.cpp");
    println!("cargo:rerun-if-changed={tp}/basis_universal");

    let mut cpp = cc::Build::new();
    cpp.cpp(true)
        .std("c++17")
        .include(tp)
        .include(format!("{tp}/basis_universal/transcoder"))
        .define("BASISD_SUPPORT_KTX2", "1")
        .define("BASISD_SUPPORT_KTX2_ZSTD", "1")
        .file(format!("{tp}/basis_universal/transcoder/basisu_transcoder.cpp"))
        .file(format!("{tp}/basis_shim.cpp"))
        .warnings(false);
    if cpp.get_compiler().is_like_msvc() {
        cpp.flag("/EHsc").flag("/utf-8");
    }
    cpp.compile("cyclops_basisu");

    cc::Build::new()
        .file(format!("{tp}/basis_universal/zstd/zstddeclib.c"))
        .warnings(false)
        .compile("cyclops_zstddec");

    windows_resources();
}

/// The exe's icon (Explorer, Start, Alt+Tab) and version information (file properties, Task
/// Manager), generated so the version always matches Cargo.toml.
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/cyclops.ico");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let icon = format!("{dir}/assets/cyclops.ico").replace('\\', "/");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<u32> = version.split(['.', '-']).filter_map(|n| n.parse().ok()).chain([0; 4]).take(4).collect();
    let [a, b, c, d] = [numbers[0], numbers[1], numbers[2], numbers[3]];
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Martin Vestergaard Madsen"
      VALUE "FileDescription", "Cyclops Image Browser"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "cyclops"
      VALUE "LegalCopyright", "© 2026 Martin Vestergaard Madsen"
      VALUE "OriginalFilename", "cyclops.exe"
      VALUE "ProductName", "Cyclops Image Browser"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("cyclops.rc");
    std::fs::write(&path, rc).unwrap();
    embed_resource::compile(&path, embed_resource::NONE).manifest_optional().unwrap();
}
