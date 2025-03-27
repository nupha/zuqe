#include <stddef.h>
#include <stdint.h>

#ifdef _MSC_VER
#include <malloc.h>
#endif


void c_alloca_callback(size_t size, void (*callback)(uint8_t *, void *), void* data) {
#ifdef _MSC_VER
    uint8_t *buffer = _alloca(size);
#else
    uint8_t buffer[size];
#endif

    return callback(&buffer[0], data);
}


uint8_t c_alloca(size_t size) {
#ifdef _MSC_VER
    uint8_t *buffer = _alloca(size);
#else
    uint8_t buffer[size];
#endif
    return &buffer[0];
}
