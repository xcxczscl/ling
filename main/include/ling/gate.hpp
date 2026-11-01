// 能力门禁。四项内核服务之一（与 router / event / depgraph 平级）。
//
// 语义见 docs/ling/04-详细设计/01-内核四服务.md §7：
//   插件在清单里声明自己需要哪些能力，注册方法时再声明这个方法需要哪些能力，
//   门禁在注册时校验「方法声明的能力 ⊆ 清单声明」，越界就拒绝（E_CAPABILITY_DENIED）。
//
// 能力位集为什么是枚举而不是字符串：注册/调用是热路径，掩码按位比较，不用逐串比对。
#pragma once

#include <cstdint>

#include "ling/manifest.hpp"

namespace ling {

// 能力位集，与 04/02 §4.4 的 requires_mask 一致。
enum CapabilityMask : unsigned {
    kCapFs = 1u,        // 读写文件
    kCapNetwork = 2u,   // 访问网络
    kCapUi = 4u,        // 界面相关
    kCapShell = 8u,     // 执行外部命令
};

// 把清单声明的能力列表转成位掩码。
std::uint32_t capability_mask_of(const Manifest& manifest);

// requires_mask 是否被 declared 完全覆盖（即这个方法没越出清单声明的能力）。
inline bool capability_allowed(std::uint32_t requires_mask, std::uint32_t declared) {
    return (requires_mask & ~declared) == 0;
}

}  // namespace ling
