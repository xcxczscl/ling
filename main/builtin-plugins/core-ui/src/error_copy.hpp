// 错误码到文案的表。
//
// 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/01-服务端与接口.md §4：
// 界面这一侧只有两个文案来源——宿主转发的 message，和这张表给的 advice。
// core-ui 自己判定的错误（E_UI_*、E_PROTOCOL、E_PAYLOAD_TOO_LARGE、E_TIMEOUT）
// 的 message 也在这张表里，免得同一件事在服务端和界面上写成两句不同的话。
#pragma once

#include <string>

namespace ling {
namespace ui {

struct Copy {
    std::string message;  // 人话原因
    std::string advice;   // 下一步做什么；空表示这条码没有「下一步」
};

// 查表。code 不在表里时返回空的两段，调用方用宿主原话兜底。
Copy copy_for(const std::string& code);

// 组装一条错误对象：code 恒有、message 恒有、advice 有才写、detail 原样透传。
// message 参数为空时用表里的；detail 为空时不写这个字段。
std::string error_json(const std::string& code, const std::string& message,
                       const std::string& detail);

}  // namespace ui
}  // namespace ling
