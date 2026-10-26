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

    libraw(tp);
    gpr(tp);
    windows_resources();
}

/// The parts of GoPro's GPR SDK (Apache-2.0/MIT; bundled Adobe DNG SDK and XMP: BSD; expat: MIT)
/// needed to convert GPR files (VC-5 compressed DNGs) to plain DNGs for LibRaw.
fn gpr(tp: &str) {
    let lib = format!("{tp}/gpr/source/lib");
    println!("cargo:rerun-if-changed={tp}/gpr");
    println!("cargo:rerun-if-changed={tp}/gpr_shim.cpp");
    let files = |dir: &str, ext: &str| -> Vec<std::path::PathBuf> {
        let mut out: Vec<_> = std::fs::read_dir(format!("{lib}/{dir}"))
            .expect("gpr sources")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .collect();
        out.sort();
        out
    };
    let includes = [
        "common/public",
        "common/private",
        "vc5_common",
        "vc5_decoder",
        "dng_sdk",
        "xmp_core/public/include",
        "expat_lib",
        "md5_lib",
        "gpr_sdk/public",
        "gpr_sdk/private",
    ];
    let configure = |build: &mut cc::Build| {
        for dir in includes {
            build.include(format!("{lib}/{dir}"));
        }
        build
            .define("XML_STATIC", "1")
            .define("GPR_READING", "1")
            .define("GPR_WRITING", "0")
            .define("GPR_JPEG_AVAILABLE", "0")
            .warnings(false);
        if build.get_compiler().is_like_msvc() {
            // A plain absolute path: cl.exe can't open canonicalize()'s \\?\ form.
            let compat = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join(tp).join("gpr_compat.h");
            build.flag(format!("/FI{}", compat.display())).define("_CRT_SECURE_NO_WARNINGS", None);
        }
    };

    let mut c = cc::Build::new();
    configure(&mut c);
    for dir in ["common/private", "vc5_common", "vc5_decoder", "expat_lib", "md5_lib"] {
        c.files(files(dir, "c"));
    }
    c.compile("cyclops_gpr_c");

    let mut cpp = cc::Build::new();
    cpp.cpp(true);
    configure(&mut cpp);
    for dir in ["dng_sdk", "xmp_core", "gpr_sdk/private"] {
        cpp.files(files(dir, "cpp"));
    }
    cpp.files(files("common/private", "cpp"));
    cpp.file(format!("{tp}/gpr_shim.cpp"));
    if cpp.get_compiler().is_like_msvc() {
        cpp.flag("/EHsc").flag("/utf-8");
    }
    cpp.compile("cyclops_gpr");
}

/// LibRaw (CDDL-1.0 / LGPL-2.1, used under the CDDL, unmodified) for camera RAW, with zlib for
/// deflate-compressed and floating-point DNGs.
fn libraw(tp: &str) {
    let root = format!("{tp}/libraw");
    println!("cargo:rerun-if-changed={root}");
    println!("cargo:rerun-if-changed={tp}/libraw_shim.cpp");
    let mut sources = vec![std::path::PathBuf::from(format!("{tp}/libraw_shim.cpp"))];
    let mut dirs = vec![std::path::PathBuf::from(format!("{root}/src"))];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("libraw sources").flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                dirs.push(path);
            } else if name.ends_with(".cpp") && !name.ends_with("_ph.cpp") {
                // *_ph.cpp are placeholders the upstream build doesn't compile either.
                sources.push(path);
            }
        }
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(&root)
        .files(&sources)
        .define("LIBRAW_NODLL", None)
        .define("LIBRAW_BUILDLIB", None)
        .define("USE_ZLIB", None)
        .warnings(false);
    if let Some(zlib) = std::env::var_os("DEP_Z_INCLUDE") {
        build.include(zlib);
    }
    if build.get_compiler().is_like_msvc() {
        build.flag("/EHsc").flag("/utf-8").define("WIN32", None).define("_CRT_SECURE_NO_WARNINGS", None);
        // Multi-threaded demosaicing and decoding: 2-4x faster develops (needs vcomp140.dll, part
        // of the Visual C++ runtime the exe already uses).
        build.flag("/openmp");
    }
    build.compile("cyclops_libraw");
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
