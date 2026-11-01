// 能力门禁实现。见 gate.hpp。
#include "ling/gate.hpp"

namespace ling {

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
    }
    return mask;
}

}  // namespace ling
