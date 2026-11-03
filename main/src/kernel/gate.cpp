// 能力门禁实现。见 gate.hpp。
#include "ling/gate.hpp"

#include <cctype>
#include <string>

namespace ling {

// 把清单里声明的能力字符串合成一个位掩码。
//
// 用位掩码而不是字符串集合，是为了注册时能 O(1) 比对「方法声明的能力是否 ⊆ 清单声明」：
// (requires_mask & ~declared) == 0 一行判定，不用遍历。能力种类固定为 fs/network/ui/shell
// 四种（04/02 §4.4），所以一个 32 位整数够用，不必表驱动。
std::uint32_t capability_mask_of(const Manifest& manifest) {
    std::uint32_t mask = 0;
    for (const auto& capability : manifest.capabilities) {
        // 比较前统一转小写：清单校验（manifest.cpp 第 10 步）也是按小写判断的，
        // 不转的话 "FS" 能通过校验，却在这里被静默忽略，方法随后撞能力不足。
        std::string text;
        text.reserve(capability.size());
        for (const char ch : capability) {
            text.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(ch))));
        }
        if (text == "fs") {
            mask |= kCapFs;
        } else if (text == "network") {
            mask |= kCapNetwork;
        } else if (text == "ui") {
            mask |= kCapUi;
        } else if (text == "shell") {
            mask |= kCapShell;
        }
        // 清单校验（manifest.cpp）已保证这里只会出现这四种，未知值静默忽略是安全的。
    }
    return mask;
}

}  // namespace ling
