// Minimal C interface to the GoPro GPR SDK for Cyclops: convert a GPR (a DNG whose raw data is
// VC-5 compressed) to a standard DNG in memory, which LibRaw can then develop.

#include <cstdint>
#include <cstdlib>
#include <cstring>

#include "gpr.h"
#include "gpr_allocator.h"
#include "gpr_buffer.h"

extern "C" {

// On success returns 1 and *out (free with cyc_gpr_free) holds the DNG.
int cyc_gpr_to_dng(const uint8_t *data, size_t len, uint8_t **out, size_t *out_len) {
    gpr_allocator allocator;
    allocator.Alloc = malloc;
    allocator.Free = free;
    // The SDK takes a non-const buffer but only reads the input.
    gpr_buffer input = {const_cast<uint8_t *>(data), len};
    gpr_buffer output = {nullptr, 0};
    gpr_parameters params;
    gpr_parameters_set_defaults(&params);
    gpr_parse_metadata(&allocator, &input, &params);
    bool ok = gpr_convert_gpr_to_dng(&allocator, &params, &input, &output);
    gpr_parameters_destroy(&params, allocator.Free);
    if (!ok || !output.buffer) {
        if (output.buffer) free(output.buffer);
        return 0;
    }
    *out = static_cast<uint8_t *>(output.buffer);
    *out_len = output.size;
    return 1;
}

void cyc_gpr_free(void *p) { free(p); }

}  // extern "C"
