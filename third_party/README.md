# Third-party code

All vendored code is unmodified and built from source by `build.rs`.

## LibRaw (`libraw/`)

Copyright (C) 2008-2025 LibRaw LLC. Dual-licensed under the GNU LGPL 2.1 or the Common
Development and Distribution License (CDDL) 1.0; Cyclops uses it under the **CDDL 1.0**
(see `libraw/LICENSE.CDDL` and `libraw/COPYRIGHT`). Version 0.22.2, unmodified; the source code
is available from https://www.libraw.org and https://github.com/LibRaw/LibRaw. LibRaw includes
code from dcraw (Dave Coffin), DCB/FBDD (Jacek Gozdz, BSD), the X3F tools (Roland Karlsson, BSD)
and the Adobe DNG SDK (MIT).

## Basis Universal transcoder (`basis_universal/transcoder`)

Copyright (C) 2019-2026 Binomial LLC. Licensed under the Apache License, Version 2.0
(see LICENSE-APACHE in the repository root). Upstream: https://github.com/BinomialLLC/basis_universal
Vendored at version 2.50, unmodified.

## Zstandard decoder (`basis_universal/zstd/zstddeclib.c`)

Copyright (c) Meta Platforms, Inc. and affiliates. BSD licence, see `basis_universal/zstd/LICENSE`.

## zlib

Built from source by the `libz-sys` crate (zlib licence, Jean-loup Gailly and Mark Adler), for
LibRaw's deflate-compressed DNGs.

## `basis_shim.cpp`, `libraw_shim.cpp`

Cyclops's own small C interfaces to the libraries above (same licence as Cyclops).
