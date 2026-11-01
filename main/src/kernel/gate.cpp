// 能力门禁实现。见 gate.hpp。
#include "ling/gate.hpp"

namespace ling {

// 把清单里声明的能力字符串合成一个位掩码。
//
// 用位掩码而不是字符串集合，是为了注册时能 O(1) 比对「方法声明的能力是否 ⊆ 清单声明」：
// (requires_mask & ~declared) == 0 一行判定，不用遍历。能力种类固定为 fs/network/ui/shell
// 四种（04/02 §4.4），所以一个 32 位整数够用，不必表驱动。
std::uint32_t capability_mask_of(const Manifest& manifest) {
    std::uint32_t mask = 0;
    for (const auto& capability : manifest.capabilities) {
        if (capability == "fs") {
            mask |= kCapFs;
        } else if (capability == "network") {
            mask |= kCapNetwork;
        } else if (capability == "ui") {
            mask |= kCapUi;
        } else if (capability == "shell") {
            mask |= kCapShell;
        }
        // 清单校验（manifest.cpp）已保证这里只会出现这四种，未知值静默忽略是安全的。
    }
    return mask;
}

}  // namespace ling
