// Minimal C interface to LibRaw for Cyclops: open a RAW from memory, read its metadata, develop
// it to linear 16-bit RGB, or extract the embedded preview.

#include <cstdint>
#include <cstring>
#include <new>

#include "libraw/libraw.h"

extern "C" {

struct CycRawInfo {
    uint32_t width, height;  // developed size, after orientation
    uint32_t bits;           // sensor bits per sample
    int32_t flip;            // LibRaw flip code (0, 3, 5, 6)
    char make[64];
    char model[64];
};

void *cyc_raw_open(const uint8_t *data, size_t len, int *error) {
    LibRaw *raw = new (std::nothrow) LibRaw(LIBRAW_OPTIONS_NONE);
    if (!raw) {
        *error = LIBRAW_UNSUFFICIENT_MEMORY;
        return nullptr;
    }
    *error = raw->open_buffer(data, len);
    if (*error != LIBRAW_SUCCESS) {
        delete raw;
        return nullptr;
    }
    return raw;
}

void cyc_raw_close(void *handle) { delete static_cast<LibRaw *>(handle); }

const char *cyc_raw_strerror(int error) { return libraw_strerror(error); }

void cyc_raw_info(void *handle, CycRawInfo *out) {
    LibRaw *raw = static_cast<LibRaw *>(handle);
    const libraw_data_t &d = raw->imgdata;
    int flip = d.sizes.flip;
    bool swap = flip == 5 || flip == 6;
    out->width = swap ? d.sizes.height : d.sizes.width;
    out->height = swap ? d.sizes.width : d.sizes.height;
    // Bits from the sensor's white level: raw_bps describes the container (16 for most DNGs)
    // or, for Phase One, the compression format.
    unsigned bits = 0;
    for (unsigned m = d.color.maximum; m; m >>= 1) bits++;
    if (bits == 0 || bits > 16) bits = d.color.raw_bps;
    out->bits = bits;
    out->flip = flip;
    std::strncpy(out->make, d.idata.make, sizeof out->make - 1);
    out->make[sizeof out->make - 1] = 0;
    std::strncpy(out->model, d.idata.model, sizeof out->model - 1);
    out->model[sizeof out->model - 1] = 0;
}

// Develop to linear 16-bit RGB (sRGB primaries, camera white balance, no auto-brightening, so
// sensor saturation is the top of the range). `half` skips demosaicing at half resolution.
// On success *image is a libraw_processed_image_t: free it with cyc_raw_free.
int cyc_raw_develop(void *handle, int half, int quality, void **image, uint32_t *width, uint32_t *height) {
    LibRaw *raw = static_cast<LibRaw *>(handle);
    libraw_output_params_t &p = raw->imgdata.params;
    p.output_bps = 16;
    p.gamm[0] = 1.0;
    p.gamm[1] = 1.0;
    p.no_auto_bright = 1;
    p.use_camera_wb = 1;
    p.use_camera_matrix = 1;
    p.output_color = 1;
    p.half_size = half;
    p.user_qual = quality;
    p.highlight = 0;
    int error = raw->unpack();
    if (error == LIBRAW_SUCCESS) error = raw->dcraw_process();
    if (error != LIBRAW_SUCCESS) return error;
    libraw_processed_image_t *mem = raw->dcraw_make_mem_image(&error);
    if (!mem) return error ? error : LIBRAW_UNSPECIFIED_ERROR;
    if (mem->type != LIBRAW_IMAGE_BITMAP || mem->colors != 3 || mem->bits != 16) {
        LibRaw::dcraw_clear_mem(mem);
        return LIBRAW_UNSPECIFIED_ERROR;
    }
    *image = mem;
    *width = mem->width;
    *height = mem->height;
    return LIBRAW_SUCCESS;
}

const uint16_t *cyc_raw_pixels(void *image) {
    return reinterpret_cast<const uint16_t *>(static_cast<libraw_processed_image_t *>(image)->data);
}

// The embedded preview: kind 1 = JPEG bytes, 2 = 8-bit RGB bitmap of width x height.
int cyc_raw_thumb(void *handle, void **image, const uint8_t **data, size_t *len, int *kind, uint32_t *width,
                  uint32_t *height) {
    LibRaw *raw = static_cast<LibRaw *>(handle);
    int error = raw->unpack_thumb();
    if (error != LIBRAW_SUCCESS) return error;
    libraw_processed_image_t *mem = raw->dcraw_make_mem_thumb(&error);
    if (!mem) return error ? error : LIBRAW_UNSPECIFIED_ERROR;
    if (mem->type == LIBRAW_IMAGE_JPEG) {
        *kind = 1;
    } else if (mem->type == LIBRAW_IMAGE_BITMAP && mem->colors == 3 && mem->bits == 8) {
        *kind = 2;
    } else {
        LibRaw::dcraw_clear_mem(mem);
        return LIBRAW_UNSPECIFIED_ERROR;
    }
    *image = mem;
    *data = mem->data;
    *len = mem->data_size;
    *width = mem->width;
    *height = mem->height;
    return LIBRAW_SUCCESS;
}

void cyc_raw_free(void *image) { LibRaw::dcraw_clear_mem(static_cast<libraw_processed_image_t *>(image)); }

}  // extern "C"
