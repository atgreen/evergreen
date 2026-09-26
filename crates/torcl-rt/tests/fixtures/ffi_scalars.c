#include <stdint.h>

/* Clang may implement widening with a plain 32-bit register move, relying
 * on caller extension. Inspect the incoming register to test that contract
 * even when this fixture is built with a compiler that extends again. */
#define NARROW_ARGUMENT(name, type) \
    uint32_t name(type value) { \
        uint32_t incoming; \
        (void)value; \
        __asm__("movl %%edi, %0" : "=r"(incoming)); \
        return incoming; \
    }
NARROW_ARGUMENT(torcl_ffi_i8, int8_t)
NARROW_ARGUMENT(torcl_ffi_u8, uint8_t)
NARROW_ARGUMENT(torcl_ffi_i16, int16_t)
NARROW_ARGUMENT(torcl_ffi_u16, uint16_t)

double torcl_ffi_mixed(int64_t a, double b, float c, int32_t d) {
    return a + b * 2.0 + c * 3.0 + d * 4.0;
}

double torcl_ffi_stack(
    uint64_t a, double b, uint64_t c, double d, uint64_t e, double f,
    uint64_t g, double h, uint64_t i, double j, uint64_t k, double l,
    uint64_t m, double n, uint64_t o, double p, double q, uint64_t r, double s
) {
    return (a + 2*c + 3*e + 4*g + 5*i + 6*k + 7*m + 8*o + 9*r)
        + b + 2*d + 3*f + 4*h + 5*j + 6*l + 7*n + 8*p + 9*q + 10*s;
}

void torcl_ffi_store(uint64_t *address, uint64_t value) {
    *address = value;
}

uint64_t torcl_ffi_seven(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
                         uint64_t e, uint64_t f, uint64_t g) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*f + 7*g;
}
