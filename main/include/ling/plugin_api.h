/* 插件边界的唯一契约。
 *
 * 依据 docs/ling/04-详细设计/02-插件装载与接口.md §4：
 * 整个系统只有 ling_plugin_entry 一个跨边界符号，其余交互都经这张函数表。
 * 这里是纯 C，不许出现 C++ 类型——跨动态库传 std::string / 虚表会「能链接、运行崩」。
 */
#ifndef LING_PLUGIN_API_H
#define LING_PLUGIN_API_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#define LING_PLUGIN_EXPORT __declspec(dllexport)
#else
#define LING_PLUGIN_EXPORT __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* 边界版本，宿主与插件必须一致，否则拒绝装载。 */
#define LING_ABI_VERSION 1u

/* 方法处理函数的签名。返回 0 表示成功，非 0 为错误码。
 *
 * 缓冲区归属（进程内轨道）：两侧共用同一个 /MD 运行时，所以统一用 malloc / free。
 * 插件用 malloc 申请 out，宿主拷走后 free；宿主给插件的缓冲区用 malloc 申请，
 * 插件用完调 free_buffer（宿主侧实现仍是 free）。S5 侧车轨道改走协议帧，不用这条。 */
typedef int32_t (*ling_method_fn)(void* handler_ctx, const uint8_t* args, size_t args_len,
                                  uint8_t** out, size_t* out_len);

/* 事件回调与撤销函数的签名。S1 只登记不使用。 */
typedef int32_t (*ling_event_fn)(void* handler_ctx, const uint8_t* payload, size_t len);
typedef void (*ling_undo_fn)(void* undo_ctx);

typedef struct {
    uint32_t abi_version;      /* 必须等于 LING_ABI_VERSION */
    const char* plugin_id;     /* 必须等于清单 id */
    const char* plugin_version;
    const char* api;           /* 接口主版本，如 "1" */
} ling_plugin_descriptor;

/* 宿主交给插件的函数表。struct_size 用于将来扩字段时保持兼容。 */
typedef struct {
    uint32_t struct_size;
    void* (*context_alloc)(const char* plugin_id, uint64_t generation);
    int32_t (*register_method)(void* ctx, const char* name, void* fn, void* undo_fn,
                               uint32_t requires_mask);
    int32_t (*unregister_method)(void* ctx, const char* name);
    int32_t (*subscribe)(void* ctx, const char* pattern, void* fn, void* undo_fn,
                         uint64_t* out_sub_id);
    int32_t (*unsubscribe)(void* ctx, uint64_t sub_id);
    int32_t (*call)(void* ctx, const char* method, const uint8_t* args, size_t args_len,
                    uint8_t** out, size_t* out_len);
    int32_t (*emit)(void* ctx, const char* topic, const uint8_t* payload, size_t len);
    int32_t (*write_config)(void* ctx, const char* key, const uint8_t* value, size_t len);
    const char* (*data_dir)(void* ctx);
    void (*free_buffer)(void* ctx, uint8_t* p, size_t len);
} ling_host_api;

/* 插件唯一导出的符号。宿主调它，读回描述符与不透明句柄。 */
typedef int32_t (*ling_plugin_entry_fn)(const ling_host_api* host, ling_plugin_descriptor* out_desc,
                                        void** out_plugin);

LING_PLUGIN_EXPORT int32_t ling_plugin_entry(const ling_host_api* host,
                                             ling_plugin_descriptor* out_desc, void** out_plugin);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* LING_PLUGIN_API_H */
