#include "ling/depgraph.hpp"

#include <algorithm>
#include <cctype>
#include <deque>
#include <set>

namespace ling {
namespace {

// 从 "0.0.1-b1" 这种宿主版本串里取前三段数字。目标版本在清单校验阶段已经保证合法，
// 这里宽松一点：遇到非数字就停下，够用且不会因为后缀出错。
std::vector<std::uint32_t> leading_numbers(const std::string& text) {
    std::vector<std::uint32_t> parts;
    std::uint32_t value = 0;
    bool has_digit = false;
    for (const char ch : text) {
        if (ch >= '0' && ch <= '9') {
            value = value * 10 + static_cast<std::uint32_t>(ch - '0');
            has_digit = true;
            continue;
        }
        if (ch == '.') {
            if (!has_digit) {
                break;
            }
            parts.push_back(value);
            value = 0;
            has_digit = false;
            continue;
        }
        break;
    }
    if (has_digit) {
        parts.push_back(value);
    }
    return parts;
}

}  // namespace

std::string DepGraph::unmet_text(const std::string& plugin_id) const {
    std::string text;
    for (const auto& item : unmet) {
        if (item.plugin_id != plugin_id) {
            continue;
        }
        if (!text.empty()) {
            text += "；";
        }
        text += item.reason;
    }
    return text;
}

bool parse_version(const std::string& text, std::vector<std::uint32_t>* out) {
    out->clear();
    std::size_t start = 0;
    for (int i = 0; i < 3; ++i) {
        const auto dot = text.find('.', start);
        const auto piece =
            text.substr(start, dot == std::string::npos ? std::string::npos : dot - start);
        if (piece.empty()) {
            return false;
        }
        std::uint64_t value = 0;
        for (const char ch : piece) {
            if (ch < '0' || ch > '9') {
                return false;
            }
            value = value * 10 + static_cast<std::uint64_t>(ch - '0');
            if (value > 65535) {
                return false;
            }
        }
        out->push_back(static_cast<std::uint32_t>(value));
        if (dot == std::string::npos) {
            return i == 2;  // 必须正好三段
        }
        start = dot + 1;
    }
    return text.find('.', start) == std::string::npos;  // 第四段就是非法
}

bool parse_requirement(const std::string& raw, Dependency* out) {
    out->raw = raw;
    out->id.clear();
    out->constraint.clear();

    const auto at = raw.find('@');
    const std::string id = at == std::string::npos ? raw : raw.substr(0, at);
    if (id.empty()) {
        return false;
    }
    for (const char ch : id) {
        const auto uch = static_cast<unsigned char>(ch);
        if (std::isalnum(uch) == 0 && ch != '-' && ch != '_') {
            return false;
        }
    }
    out->id = id;
    if (at == std::string::npos) {
        return true;
    }

    const std::string constraint = raw.substr(at + 1);
    if (constraint.empty()) {
        return false;
    }
    std::size_t start = 0;
    while (true) {
        const auto dot = constraint.find('.', start);
        const auto piece =
            constraint.substr(start, dot == std::string::npos ? std::string::npos : dot - start);
        if (piece.empty()) {
            return false;
        }
        std::uint64_t value = 0;
        for (const char ch : piece) {
            if (ch < '0' || ch > '9') {
                return false;
            }
            value = value * 10 + static_cast<std::uint64_t>(ch - '0');
            if (value > 65535) {
                return false;
            }
        }
        out->constraint.push_back(static_cast<std::uint32_t>(value));
        if (out->constraint.size() > 3) {
            return false;
        }
        if (dot == std::string::npos) {
            break;
        }
        start = dot + 1;
    }
    return true;
}

bool version_satisfies(const std::vector<std::uint32_t>& constraint, const std::string& version) {
    if (constraint.empty()) {
        return true;
    }
    const auto target = leading_numbers(version);
    if (target.size() < 3) {
        return false;
    }
    for (std::size_t i = 0; i < constraint.size(); ++i) {
        if (i >= 3 || constraint[i] != target[i]) {
            return false;
        }
    }
    return true;
}

DepGraph build_graph(const std::vector<InstalledEntry>& installed) {
    DepGraph graph;

    std::map<std::string, const InstalledEntry*> by_id;
    for (const auto& entry : installed) {
        by_id[entry.id] = &entry;
        graph.edges[entry.id];  // 让没有依赖的插件也作为节点存在
    }

    for (const auto& entry : installed) {
        for (const auto& raw : entry.requires) {
            Dependency dependency;
            if (!parse_requirement(raw, &dependency)) {
                // 清单校验阶段会拦住，这里只兜底：记成未满足，不炸图。
                graph.unmet.push_back(
                    {entry.id, raw, "依赖写法不对：" + raw});
                continue;
            }
            const auto found = by_id.find(dependency.id);
            if (found == by_id.end()) {
                graph.unmet.push_back(
                    {entry.id, raw, "缺 " + raw + "（没装这个插件）"});
                continue;
            }
            if (!version_satisfies(dependency.constraint, found->second->version)) {
                graph.unmet.push_back({entry.id, raw,
                                       "版本不符：要 " + raw + "，装的是 " +
                                           found->second->id + " " + found->second->version});
                continue;
            }
            auto& list = graph.edges[entry.id];
            if (std::find(list.begin(), list.end(), dependency.id) == list.end()) {
                list.push_back(dependency.id);
                graph.reverse[dependency.id].push_back(entry.id);
            }
        }
    }
    return graph;
}

std::vector<std::string> find_cycle(const DepGraph& graph) {
    // Kahn：反复摘掉入度为 0 的点，摘不完就是有环。摘剩下的点里必然有环，
    // 从任一个点沿边走下去直到重复，就得到一条具体的环路。
    std::map<std::string, int> in_degree;
    for (const auto& node : graph.edges) {
        in_degree[node.first];  // 保证节点存在
    }
    for (const auto& node : graph.edges) {
        for (const auto& target : node.second) {
            ++in_degree[target];
        }
    }

    std::deque<std::string> ready;
    for (const auto& node : in_degree) {
        if (node.second == 0) {
            ready.push_back(node.first);
        }
    }
    std::set<std::string> removed;
    while (!ready.empty()) {
        const std::string current = ready.front();
        ready.pop_front();
        removed.insert(current);
        const auto edges = graph.edges.find(current);
        if (edges == graph.edges.end()) {
            continue;
        }
        for (const auto& target : edges->second) {
            if (--in_degree[target] == 0) {
                ready.push_back(target);
            }
        }
    }

    if (removed.size() == graph.edges.size()) {
        return {};
    }

    // 剩下的点里找一个环。每个点只依赖一个后继，走到底必然重复。
    std::string start;
    for (const auto& node : graph.edges) {
        if (removed.count(node.first) == 0) {
            start = node.first;
            break;
        }
    }
    std::vector<std::string> path;
    std::map<std::string, std::size_t> seen;
    std::string current = start;
    while (true) {
        const auto where = seen.find(current);
        if (where != seen.end()) {
            path.erase(path.begin(), path.begin() + static_cast<std::ptrdiff_t>(where->second));
            path.push_back(current);
            return path;
        }
        seen[current] = path.size();
        path.push_back(current);
        const auto edges = graph.edges.find(current);
        if (edges == graph.edges.end() || edges->second.empty()) {
            return path;  // 不该发生，兜底
        }
        current = edges->second.front();
    }
}

std::vector<std::string> topological_order(const DepGraph& graph) {
    if (!find_cycle(graph).empty()) {
        return {};
    }
    // 被依赖者在前：needs 记「本插件要等几个被依赖者先就位」，
    // dependents 记「谁在等我」，摘掉一个点就把等待者的计数减一。
    std::map<std::string, int> needs;
    std::map<std::string, std::vector<std::string>> dependents;
    for (const auto& node : graph.edges) {
        needs[node.first] += static_cast<int>(node.second.size());
        for (const auto& target : node.second) {
            dependents[target].push_back(node.first);
        }
    }

    std::deque<std::string> ready;
    for (const auto& node : graph.edges) {
        if (needs[node.first] == 0) {
            ready.push_back(node.first);
        }
    }

    std::vector<std::string> order;
    while (!ready.empty()) {
        // 同一层按名字排序，让输出稳定，便于测试与日志对照。
        std::sort(ready.begin(), ready.end());
        const std::string current = ready.front();
        ready.pop_front();
        order.push_back(current);
        for (const auto& dependent : dependents[current]) {
            if (--needs[dependent] == 0) {
                ready.push_back(dependent);
            }
        }
    }
    if (order.size() != graph.edges.size()) {
        return {};  // 有环，上面已经拦过，兜底
    }
    return order;
}

std::vector<PlanStep> compute_plan(const DepGraph& graph,
                                  const std::map<std::string, bool>& active) {
    std::vector<PlanStep> plan;

    const auto order = topological_order(graph);
    if (order.empty()) {
        return plan;  // 有环时调用方先报错，这里不产出半截计划
    }

    // 先停：依赖不满足却还是激活态。反向拓扑序，先停依赖别人的。
    for (auto it = order.rbegin(); it != order.rend(); ++it) {
        const bool is_active = active.count(*it) != 0 && active.at(*it);
        if (is_active && !graph.unmet_text(*it).empty()) {
            plan.push_back({false, *it});
        }
    }

    // 再起：需求满足却还没激活。
    for (const auto& id : order) {
        const bool is_active = active.count(id) != 0 && active.at(id);
        if (!is_active && graph.unmet_text(id).empty()) {
            plan.push_back({true, id});
        }
    }
    return plan;
}

}  // namespace ling
