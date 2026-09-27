#include <stdint.h>

typedef double (*mixed_callback)(int8_t, uint16_t, uint64_t, float, double,
                                void *, int64_t, int32_t, int32_t,
                                double, double, double, double,
                                double, double, double, double);
double torcl_callback_mixed(mixed_callback callback) {
    return callback(-7, 65530, UINT64_MAX, 1.25f, -0.0, (void *)0x12340,
                    -9, 17, 23, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0);
}
uint64_t torcl_callback_alternate(uint64_t (*a)(void), uint64_t (*b)(void)) {
    uint64_t first = a(), second = b();
    return first * 10000 + second * 100 + a();
}
uint64_t torcl_callback_float(float (*callback)(float)) {
    union { float number; uint32_t bits; } result = { .number = callback(3.5f) };
    return result.bits;
}
uint64_t torcl_callback_signed(int8_t (*callback)(int8_t)) { return (int64_t)callback(-7); }
uint64_t torcl_callback_pointer(void *(*callback)(void *)) { return (uintptr_t)callback((void *)0x12340); }
uint64_t torcl_callback_void(void (*callback)(uint32_t)) { callback(37); return 42; }

#define VALUES_16 1ULL,2ULL,3ULL,4ULL,5ULL,6ULL,7ULL,8ULL,9ULL,10ULL,11ULL,12ULL,13ULL,14ULL,15ULL,16ULL
#define VALUES_64 VALUES_16,VALUES_16,VALUES_16,VALUES_16
#define VALUES_256 VALUES_64,VALUES_64,VALUES_64,VALUES_64
#define VALUES_1024 VALUES_256,VALUES_256,VALUES_256,VALUES_256
#define TYPES_16 uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t,uint64_t
#define TYPES_64 TYPES_16,TYPES_16,TYPES_16,TYPES_16
#define TYPES_256 TYPES_64,TYPES_64,TYPES_64,TYPES_64
#define TYPES_1024 TYPES_256,TYPES_256,TYPES_256,TYPES_256
uint64_t torcl_callback_large(uint64_t (*callback)(TYPES_1024)) {
    return callback(VALUES_1024);
}
