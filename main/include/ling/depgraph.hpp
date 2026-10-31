// 依赖图与调和计算的纯函数部分。
//
// 语义依据 docs/ling/04-详细设计/04-依赖解析与调和.md：
//   3. 版本约束匹配   —— 逐段相等，不是「大于等于」
//   4. 依赖图         —— 邻接表 + 反向表，每次从零重建
//   5. 计划计算       —— 三态判定、拓扑序、先停后起
//
// 这里只有计算，不碰装载表也不碰插件：把这一层做成纯函数，是因为它最容易出错
// （环、漏边、顺序），而纯函数能用单测几十个用例压住。执行在 Kernel 里。
#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <vector>

namespace ling {

// 一条依赖声明，形如 "demo-a@1.2"。
struct Dependency {
    std::string id;
    std::vector<std::uint32_t> constraint;  // 空表示不限版本
    std::string raw;                        // 原始写法，报错时原样给人看
};

// 构建依赖图时需要的「已安装插件」快照。
struct InstalledEntry {
    std::string id;
    std::string version;
    std::vector<std::string> requires;  // 清单里 [requires] plugins 的原文
};

// 一条没被满足的需求。reason 是给人看的一句话，直接进内核日志与插件状态。
struct UnmetRequirement {
    std::string plugin_id;
    std::string required;
    std::string reason;
};

struct DepGraph {
    // 需求方 → 它依赖的插件（只收满足的那部分）
    std::map<std::string, std::vector<std::string>> edges;
    // 被依赖方 → 谁依赖它
    std::map<std::string, std::vector<std::string>> reverse;
    std::vector<UnmetRequirement> unmet;

    // 某个插件未满足的需求，拼成一句给人看的话；空表示全部满足。
    std::string unmet_text(const std::string& plugin_id) const;
};

// 解析 "a@1.2"，失败返回 false（空 id、段数超过 3、段内非数字）。
bool parse_requirement(const std::string& raw, Dependency* out);

// 三段式版本解析。失败表示写法不合法，调用方按「不满足」处理。
bool parse_version(const std::string& text, std::vector<std::uint32_t>* out);

// 约束与目标版本是否匹配（04/04 §3.2）。
bool version_satisfies(const std::vector<std::uint32_t>& constraint, const std::string& version);

// 从零构建。缺插件与版本不符都记进 unmet，不加边。
DepGraph build_graph(const std::vector<InstalledEntry>& installed);

// 环检测（Kahn）。返回空表示无环；否则返回一个环路，首尾相同。
std::vector<std::string> find_cycle(const DepGraph& graph);

// 拓扑序：被依赖者在前。有环时返回空。
std::vector<std::string> topological_order(const DepGraph& graph);

struct PlanStep {
    bool activate = true;  // true = 激活，false = 停用
    std::string plugin_id;
};

// 三态判定后的计划：先全部停用（反向拓扑序），再全部激活（拓扑序）。
// active 说明每个已安装插件当前是否处于激活态；不在表里的按未激活处理。
std::vector<PlanStep> compute_plan(const DepGraph& graph,
                                   const std::map<std::string, bool>& active);

}  // namespace ling
