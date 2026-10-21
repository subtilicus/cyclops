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
}
