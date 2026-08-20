#ifndef RISH_TCTI_PROVIDER_H
#define RISH_TCTI_PROVIDER_H

#include <stddef.h>
#include <stdint.h>

#define RISH_TCTI_ABI_VERSION_V1 1u
#define RISH_TCTI_STATUS_OK 0

#define RISH_TCTI_FEATURE_TCTI (UINT64_C(1) << 0)
#define RISH_TCTI_FEATURE_FULL_SYSTEM (UINT64_C(1) << 1)
#define RISH_TCTI_FEATURE_X86_64 (UINT64_C(1) << 2)
#define RISH_TCTI_FEATURE_BOUNDED_RUN (UINT64_C(1) << 3)
#define RISH_TCTI_FEATURE_CANCEL_POLL (UINT64_C(1) << 4)
#define RISH_TCTI_FEATURE_SERIAL_16550 (UINT64_C(1) << 5)
#define RISH_TCTI_FEATURE_VIRTIO_BLOCK (UINT64_C(1) << 6)
#define RISH_TCTI_FEATURE_INITRD (UINT64_C(1) << 7)
#define RISH_TCTI_FEATURE_USER_NETWORK (UINT64_C(1) << 8)
#define RISH_TCTI_FEATURE_CONTROL_SERIAL (UINT64_C(1) << 9)

#define RISH_TCTI_FEATURE_JIT (UINT64_C(1) << 48)
#define RISH_TCTI_FEATURE_HVF (UINT64_C(1) << 49)
#define RISH_TCTI_FEATURE_KVM (UINT64_C(1) << 50)
#define RISH_TCTI_FEATURE_PRIVATE_API (UINT64_C(1) << 51)
#define RISH_TCTI_FEATURE_EXECUTABLE_MEMORY (UINT64_C(1) << 52)

#define RISH_TCTI_NETWORK_DISABLED 0u
#define RISH_TCTI_NETWORK_USER_NAT 1u

#define RISH_TCTI_MACHINE_RUNNING 0u
#define RISH_TCTI_MACHINE_HALTED 1u
#define RISH_TCTI_MACHINE_STOPPED 2u
#define RISH_TCTI_MACHINE_FAULTED 3u

typedef struct {
    const uint8_t *data;
    size_t len;
} RishTctiSliceV1;

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    uint8_t qemu_version[16];
    uint8_t source_revision[41];
    uint8_t build_id[65];
    uint8_t guest_architecture[16];
    uint8_t host_architecture[16];
    uint8_t target_list[32];
    uint64_t compiled_features;
    uint32_t min_memory_mib;
    uint32_t max_memory_mib;
    uint32_t max_vcpus;
} RishTctiBuildInfoV1;

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    uint32_t memory_mib;
    uint32_t vcpus;
    RishTctiSliceV1 kernel_path;
    RishTctiSliceV1 initrd_path;
    RishTctiSliceV1 root_disk_path;
    uint32_t network_mode;
} RishTctiConfigV1;

typedef size_t (*RishTctiSerialWriteV1)(void *context,
                                        const uint8_t *data,
                                        size_t len);
typedef size_t (*RishTctiSerialReadV1)(void *context,
                                       uint8_t *data,
                                       size_t capacity);
typedef uint8_t (*RishTctiShouldCancelV1)(void *context);

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    void *context;
    RishTctiSerialWriteV1 serial_write;
    RishTctiSerialReadV1 serial_read;
    RishTctiSerialWriteV1 control_write;
    RishTctiSerialReadV1 control_read;
    RishTctiShouldCancelV1 should_cancel;
} RishTctiHostCallbacksV1;

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    uint32_t state;
    uint8_t pc_valid;
    uint8_t reserved[7];
    uint64_t pc;
    uint64_t total_units;
} RishTctiSnapshotV1;

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    uint64_t executed_units;
    RishTctiSnapshotV1 snapshot;
} RishTctiRunResultV1;

typedef struct {
    size_t struct_size;
    uint32_t abi_version;
    int32_t (*build_info)(RishTctiBuildInfoV1 *output);
    int32_t (*create)(const RishTctiConfigV1 *config,
                      const RishTctiHostCallbacksV1 *callbacks,
                      void **output);
    int32_t (*snapshot)(void *handle, RishTctiSnapshotV1 *output);
    int32_t (*run_quantum)(void *handle,
                           uint64_t max_units,
                           RishTctiRunResultV1 *output);
    int32_t (*request_stop)(void *handle);
    void (*destroy)(void *handle);
} RishTctiApiV1;

/*
 * serial_* carries the 16550A console (ttyS0). control_* carries a second
 * 16550A (ttyS1) used exclusively for versioned rish guest protocol frames;
 * a provider that drops control bytes must surface the drop to the host so
 * the adapter can fail closed.
 *
 * The provider must copy config paths during create(). The callback table and
 * context remain valid until destroy(). run_quantum() must be bounded and poll
 * should_cancel(). The reviewed build is:
 *
 *   qemu 10.0.2, tag v10.0.2-utm
 *   commit 37ba092d59aff24900dfd0d5e01d4ed68441ba07
 *   target-list=x86_64-softmmu
 *   host=aarch64
 *   --enable-tcg-threaded-interpreter
 *
 * JIT, executable-memory translation, HVF, KVM, and private APIs are forbidden.
 * QEMU and linked dependencies carry licenses independent of this MIT adapter;
 * distributors must ship the corresponding source and notices.
 */

#endif
