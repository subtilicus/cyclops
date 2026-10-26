/* Force-included when building the GPR SDK with MSVC: its bundled expat uses the GCC-only
   __attribute((fallthrough)) unconditionally. */
#ifdef _MSC_VER
#define __attribute__(x)
#define __attribute(x)
#endif
