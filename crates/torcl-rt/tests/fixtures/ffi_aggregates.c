#include <stdint.h>
#include <stdarg.h>

typedef struct { uint64_t a, b; } II;
typedef struct { double a, b; } SS;
typedef struct { uint64_t a; double b; } IS;
typedef struct { double a; uint64_t b; } SI;
typedef struct { uint64_t a, b, c; } Big;
typedef struct __attribute__((packed)) { uint8_t a; double b; } Packed;
typedef struct { uint8_t a, b, c; } Tiny;
typedef struct { struct { float a, b; } pair; uint64_t c; } Nested;
typedef union { uint64_t u; double d; } U;
typedef struct { U u; float a, b; } UnionPair;
typedef struct { uint8_t prefix[7]; Packed value; } Realigned;
typedef struct { uint8_t a; } Small1;
typedef struct { uint16_t a; } Small2;
typedef struct { float a; } Float1;
typedef struct { float a, b; } Float2;

Small1 aggregate_small1(Small1 x) { return (Small1){x.a + 3}; }
Small2 aggregate_small2(Small2 x) { return (Small2){x.a + 1000}; }
Float1 aggregate_float1(Float1 x) { return (Float1){x.a * 2}; }
Float2 aggregate_float2(Float2 x) { return (Float2){x.b + 1, x.a + 2}; }
U aggregate_union8(U x) { x.u ^= UINT64_C(0x123456789abcdef0); return x; }
uint64_t aggregate_copy_alignment(Big x) {
    uintptr_t location = (uintptr_t)&x;
    volatile Big *copy = &x;
    copy->a = 0; copy->b = 0; copy->c = 0;
    return location & 15;
}
Big aggregate_sret_mixed(double a, uint64_t b, float c, Big d, double e) {
    return (Big){d.a + (uint64_t)a, d.b + b + (uint64_t)c, d.c + (uint64_t)e};
}
Big aggregate_variadic_sret(float fixed, int count, ...) {
    va_list ap; va_start(ap, count);
    Big result = {(uint64_t)fixed, 0, 0};
    for (int i = 0; i < count; i++) {
        result.b += (uint64_t)va_arg(ap, double);
        II pair = va_arg(ap, II);
        result.c += pair.a + pair.b;
    }
    va_end(ap); return result;
}

II aggregate_ii(II x, II y) { return (II){x.a + y.b, x.b ^ y.a}; }
SS aggregate_ss(SS x) { return (SS){x.b + 1.25, x.a - 2.5}; }
IS aggregate_is(IS x) { return (IS){x.a + 11, x.b * 2}; }
SI aggregate_si(SI x) { return (SI){x.a * 3, x.b + 17}; }
Big aggregate_big(Big x, uint64_t n) { return (Big){x.a + n, x.b + 2*n, x.c + 3*n}; }
Packed aggregate_packed(Packed x) { x.a += 1; x.b *= 2; return x; }
Tiny aggregate_tiny(Tiny x) { return (Tiny){x.c, x.a, x.b}; }
Nested aggregate_nested(Nested x) { x.pair.a += 1; x.pair.b += 2; x.c += 3; return x; }
UnionPair aggregate_union(UnionPair x) { x.u.u += 1; x.a += 2; x.b += 3; return x; }
Realigned aggregate_realigned(Realigned x) {
    for (int i=0;i<7;i++) x.prefix[i] += i;
    x.value.a += 2; x.value.b *= 3; return x;
}

uint64_t aggregate_gpr_rollback(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
                              uint64_t e, II pair, uint64_t tail) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*pair.a + 7*pair.b + 8*tail;
}
double aggregate_sse_rollback(double a, double b, double c, double d, double e,
                             double f, double g, SS pair, double tail) {
    return a + b + c + d + e + f + g + 2*pair.a + 3*pair.b + 4*tail;
}
double aggregate_mixed_rollback(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
                               uint64_t e, uint64_t f, SI pair, double tail) {
    return a + b + c + d + e + f + pair.a + pair.b + tail;
}
Big aggregate_sret_pressure(uint64_t a, uint64_t b, uint64_t c, uint64_t d,
                            uint64_t e, II pair, uint64_t tail) {
    return (Big){a+b+c+d+e, pair.a+pair.b, tail};
}
double aggregate_variadic(int count, ...) {
    va_list ap; va_start(ap, count);
    double total = 0;
    for (int i = 0; i < count; ++i) {
        IS pair = va_arg(ap, IS);
        double value = va_arg(ap, double);
        int small = va_arg(ap, int);
        total += pair.a + pair.b + value + small;
    }
    va_end(ap); return total;
}
Big aggregate_callback(double (*callback)(double), int *returned) {
    double answer = callback(1.25);
    *returned = 99;
    return (Big){(uint64_t)answer, 2, 3};
}
