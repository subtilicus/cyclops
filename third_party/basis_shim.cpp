// Minimal C interface to Basis Universal's KTX2 transcoder for Cyclops.
// Transcodes ETC1S / UASTC (LDR) to RGBA8 and UASTC/ASTC HDR to RGBA half floats.

#include "basis_universal/transcoder/basisu_transcoder.h"

#include <cstdint>
#include <new>

using namespace basist;

extern "C" {

// Must be called once (Cyclops guards it with std::sync::Once) before anything else.
void cyc_basis_init() { basisu_transcoder_init(); }

// Returns an opaque handle, or null if the data isn't a transcodable KTX2 file.
void* cyc_ktx2_open(const uint8_t* data, uint32_t len) {
    auto* t = new (std::nothrow) ktx2_transcoder();
    if (!t) return nullptr;
    if (!t->init(data, len) || !t->start_transcoding()) {
        delete t;
        return nullptr;
    }
    return t;
}

void cyc_ktx2_close(void* handle) { delete static_cast<ktx2_transcoder*>(handle); }

// kind: 0 = ETC1S, 1 = UASTC LDR, 2 = HDR (UASTC/ASTC HDR), 3 = other LDR (ASTC/XUASTC/XUBC7).
void cyc_ktx2_info(void* handle, uint32_t* levels, uint32_t* layers, uint32_t* faces, int* kind) {
    auto* t = static_cast<ktx2_transcoder*>(handle);
    *levels = t->get_levels() ? t->get_levels() : 1;
    *layers = t->get_layers() ? t->get_layers() : 1;
    *faces = t->get_faces() ? t->get_faces() : 1;
    *kind = t->is_etc1s() ? 0 : t->is_uastc() ? 1 : t->is_hdr() ? 2 : 3;
}

// Size of one image in pixels; returns 0 if the level/layer/face doesn't exist.
int cyc_ktx2_image_size(void* handle, uint32_t level, uint32_t layer, uint32_t face, uint32_t* w, uint32_t* h) {
    ktx2_image_level_info info;
    if (!static_cast<ktx2_transcoder*>(handle)->get_image_level_info(info, level, layer, face)) return 0;
    *w = info.m_orig_width;
    *h = info.m_orig_height;
    return 1;
}

// Transcode one image into `out` (w*h pixels): RGBA8, or RGBA half floats when `hdr` is set.
int cyc_ktx2_transcode(void* handle, uint32_t level, uint32_t layer, uint32_t face, void* out, uint32_t pixels, int hdr) {
    auto fmt = hdr ? transcoder_texture_format::cTFRGBA_HALF : transcoder_texture_format::cTFRGBA32;
    return static_cast<ktx2_transcoder*>(handle)->transcode_image_level(level, layer, face, out, pixels, fmt) ? 1 : 0;
}

}  // extern "C"
