// jemalloc extent_hooks demo: 4GB virtual heap
//
// Build:
//   git clone https://github.com/jemalloc/jemalloc.git
//   cd jemalloc && ./autogen.sh && make -j$(nproc)
//   gcc -I include -L lib -ljemalloc -Wl,-rpath,lib virtual_heap_demo.c -o demo
//
// Or with prefix:
//   ./configure --with-jemalloc-prefix=je_
//   gcc -I include -L lib -l:libjemalloc.a virtual_heap_demo.c -o demo

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <pthread.h>
#include <sys/mman.h>
#include <jemalloc/jemalloc.h>

// ---------------------------------------------------------------------------
// Virtual heap: 4GB contiguous address space
// ---------------------------------------------------------------------------
#define VIRTUAL_HEAP_SIZE (4ULL * 1024 * 1024 * 1024)

static struct {
    uint8_t *base;      // base address of the 4GB mapping
    uint8_t *cursor;    // bump pointer (next free byte)
    size_t   capacity;  // = VIRTUAL_HEAP_SIZE
} g_vheap;

// ---------------------------------------------------------------------------
// extent_hooks: the 9 function pointers jemalloc calls
// ---------------------------------------------------------------------------
static void *
vheap_alloc(extent_hooks_t *h, void *new_addr, size_t size,
            size_t align, bool *zero, bool *commit, unsigned arena_ind)
{
    // jemalloc may request a specific address for relocation; we can't honor that
    if (new_addr != NULL)
        return NULL;

    // Align cursor up
    uintptr_t cur = (uintptr_t)g_vheap.cursor;
    uintptr_t aligned = (cur + align - 1) & ~(align - 1);
    size_t padding = aligned - cur;

    if (aligned + size > g_vheap.base + g_vheap.capacity)
        return NULL;    // out of virtual heap

    uint8_t *ret = (uint8_t *)aligned;
    g_vheap.cursor = ret + size;

    if (*zero)
        memset(ret, 0, size);

    // We return pre-committed memory (the mmap was PROT_READ|PROT_WRITE)
    *commit = true;
    return ret;
}

static bool
vheap_dalloc(extent_hooks_t *h, void *addr, size_t size,
             bool committed, unsigned arena_ind)
{
    // We never release pages back to OS; just refuse and let jemalloc retain.
    return true;    // failure = "not implemented"
}

static bool
vheap_destroy(extent_hooks_t *h, void *addr, size_t size,
              bool committed, unsigned arena_ind)
{
    return true;    // never destroy
}

static bool
vheap_commit(extent_hooks_t *h, void *addr, size_t size,
             size_t offset, size_t length, unsigned arena_ind)
{
    // Already committed at mmap time.
    return false;   // success
}

static bool
vheap_decommit(extent_hooks_t *h, void *addr, size_t size,
               size_t offset, size_t length, unsigned arena_ind)
{
    // We don't decommit; refuse and let jemalloc retain.
    return true;
}

static bool
vheap_purge(extent_hooks_t *h, void *addr, size_t size,
            size_t offset, size_t length, unsigned arena_ind)
{
    // madvise(MADV_DONTNEED) would release physical pages but keep VM.
    // For a virtual heap we don't want that.
    return true;    // refuse
}

static bool
vheap_split(extent_hooks_t *h, void *addr, size_t size,
            size_t size_a, size_t size_b, bool committed,
            unsigned arena_ind)
{
    // Split is always possible in a contiguous buffer — just return success.
    // jemalloc manages the bookkeeping; we don't need to do anything.
    return false;   // success
}

static bool
vheap_merge(extent_hooks_t *h, void *addr_a, size_t size_a,
            void *addr_b, size_t size_b, bool committed,
            unsigned arena_ind)
{
    // Merge is always possible since everything is in one contiguous region.
    return false;   // success
}

static extent_hooks_t g_hooks = {
    vheap_alloc,
    vheap_dalloc,
    vheap_destroy,
    vheap_commit,
    vheap_decommit,
    vheap_purge,    // purge_lazy
    vheap_purge,    // purge_forced
    vheap_split,
    vheap_merge,
};

// ---------------------------------------------------------------------------
// Helper: create arena backed by virtual heap, return arena index
// ---------------------------------------------------------------------------
static unsigned
create_vheap_arena(void)
{
    unsigned arena_ind;
    size_t sz = sizeof(arena_ind);
    extent_hooks_t *hp = &g_hooks;

    if (je_mallctl("arenas.create", &arena_ind, &sz, &hp, sizeof(hp))) {
        fprintf(stderr, "arenas.create failed\n");
        exit(1);
    }

    // Disable decay so jemalloc never tries to purge/return pages
    ssize_t no_decay = -1;
    char cmd[64];
    snprintf(cmd, sizeof(cmd), "arena.%u.dirty_decay_ms", arena_ind);
    je_mallctl(cmd, NULL, NULL, &no_decay, sizeof(no_decay));
    snprintf(cmd, sizeof(cmd), "arena.%u.muzzy_decay_ms", arena_ind);
    je_mallctl(cmd, NULL, NULL, &no_decay, sizeof(no_decay));

    return arena_ind;
}

// ---------------------------------------------------------------------------
// Demo
// ---------------------------------------------------------------------------
int
main(void)
{
    // 1. Reserve 4GB contiguous virtual address space (not committed yet)
    g_vheap.base = mmap(NULL, VIRTUAL_HEAP_SIZE,
                        PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (g_vheap.base == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    g_vheap.cursor = g_vheap.base;
    g_vheap.capacity = VIRTUAL_HEAP_SIZE;

    printf("virtual heap: %p – %p  (%llu MB)\n",
           g_vheap.base,
           g_vheap.base + VIRTUAL_HEAP_SIZE,
           (unsigned long long)(VIRTUAL_HEAP_SIZE >> 20));

    // 2. Create jemalloc arena with our hooks
    unsigned arena = create_vheap_arena();
    int flags = MALLOCX_ARENA(arena) | MALLOCX_TCACHE_NONE;

    printf("arena %u created, flags=0x%x\n", arena, flags);

    // 3. Allocate some objects — they all live inside g_vheap
    #define N 10
    void *ptrs[N];
    for (int i = 0; i < N; i++) {
        size_t sz = 16 + i * 8;     // varying sizes
        ptrs[i] = je_mallocx(sz, flags);
        if (!ptrs[i]) {
            fprintf(stderr, "mallocx(%zu) failed at i=%d\n", sz, i);
            return 1;
        }
        printf("  alloc[%d] size=%3zu  ptr=%p  offset=+%llu\n",
               i, sz, ptrs[i],
               (unsigned long long)((uint8_t *)ptrs[i] - g_vheap.base));
    }

    // 4. Verify all pointers are inside [base, base+4GB)
    for (int i = 0; i < N; i++) {
        uint8_t *p = (uint8_t *)ptrs[i];
        if (p < g_vheap.base || p >= g_vheap.base + VIRTUAL_HEAP_SIZE) {
            fprintf(stderr, "ptrs[%d]=%p OUT OF RANGE!\n", i, p);
            return 1;
        }
    }
    printf("all %d pointers within virtual heap — OK\n", N);

    // 5. Free half, reallocate, verify bump pointer never goes backward
    uint8_t *cursor_before = g_vheap.cursor;
    for (int i = 0; i < N; i += 2)
        je_dallocx(ptrs[i], flags);

    for (int i = 0; i < N; i += 2) {
        ptrs[i] = je_mallocx(16 + i * 8, flags);
        if (!ptrs[i]) {
            fprintf(stderr, "realloc failed\n");
            return 1;
        }
    }
    printf("cursor before free-realloc: %p, after: %p (grew by %llu bytes)\n",
           cursor_before, g_vheap.cursor,
           (unsigned long long)(g_vheap.cursor - cursor_before));

    // 6. Show stats
    je_mallctl("stats.print", NULL, NULL, NULL, 0);

    // Cleanup (optional — in real use you'd munmap the vheap)
    munmap(g_vheap.base, VIRTUAL_HEAP_SIZE);
    return 0;
}
