// 依赖图与调和计算的单元测试。
// 依据 docs/ling/04-详细设计/04-依赖解析与调和.md §3 至 §5。
#include <catch_amalgamated.hpp>

#include <map>
#include <string>
#include <vector>

#include "ling/depgraph.hpp"

using namespace ling;

namespace {

InstalledEntry entry(const std::string& id, const std::string& version,
                     const std::vector<std::string>& requires = {}) {
    InstalledEntry item;
    item.id = id;
    item.version = version;
    item.requires = requires;
    return item;
}

}  // namespace

TEST_CASE("依赖写法解析", "[depgraph]") {
    Dependency dependency;

    REQUIRE(parse_requirement("demo-a", &dependency));
    CHECK(dependency.id == "demo-a");
    CHECK(dependency.constraint.empty());

    REQUIRE(parse_requirement("demo-a@1", &dependency));
    CHECK(dependency.constraint == std::vector<std::uint32_t>{1});

    REQUIRE(parse_requirement("demo-a@1.2.3", &dependency));
    CHECK(dependency.constraint == std::vector<std::uint32_t>{1, 2, 3});

    // 前导零按整数解析，01 与 1 等价（04/04 §3.3 最后一条）
    REQUIRE(parse_requirement("demo-a@01", &dependency));
    CHECK(dependency.constraint == std::vector<std::uint32_t>{1});

    CHECK_FALSE(parse_requirement("", &dependency));
    CHECK_FALSE(parse_requirement("@1", &dependency));
    CHECK_FALSE(parse_requirement("demo-a@", &dependency));
    CHECK_FALSE(parse_requirement("demo-a@1.2.3.4", &dependency));
    CHECK_FALSE(parse_requirement("demo-a@x", &dependency));
    CHECK_FALSE(parse_requirement("demo a", &dependency));
}

TEST_CASE("版本约束逐段相等", "[depgraph]") {
    CHECK(version_satisfies({}, "1.2.3"));
    CHECK(version_satisfies({1}, "1.2.3"));
    CHECK(version_satisfies({1, 2}, "1.2.9"));
    CHECK(version_satisfies({1, 2, 3}, "1.2.3"));

    CHECK_FALSE(version_satisfies({1}, "2.0.0"));
    CHECK_FALSE(version_satisfies({1, 2}, "1.3.0"));
    CHECK_FALSE(version_satisfies({1, 2, 3}, "1.2.4"));
    // 目标版本写法不合法时按不满足处理，不抛异常
    CHECK_FALSE(version_satisfies({1}, "1.2"));
}

TEST_CASE("依赖图记录边与未满足", "[depgraph]") {
    const auto graph = build_graph({entry("demo-a", "1.0.0"), entry("demo-b", "1.0.0", {"demo-a@1"}),
                                    entry("demo-c", "1.0.0", {"demo-x@1"}),
                                    entry("demo-d", "1.0.0", {"demo-a@2"})});

    CHECK(graph.edges.at("demo-b") == std::vector<std::string>{"demo-a"});
    CHECK(graph.reverse.at("demo-a") == std::vector<std::string>{"demo-b"});

    CHECK(graph.unmet_text("demo-a").empty());
    CHECK(graph.unmet_text("demo-b").empty());
    CHECK(graph.unmet_text("demo-c").find("缺 demo-x@1") != std::string::npos);
    CHECK(graph.unmet_text("demo-d").find("版本不符") != std::string::npos);
}

TEST_CASE("依赖不可用时往下传", "[depgraph]") {
    // demo-a 装着但被停用：demo-b 要它，demo-c 又要 demo-b，两个都得停。
    const auto graph = build_graph({entry("demo-a", "1.0.0"),
                                    entry("demo-b", "1.0.0", {"demo-a@1"}),
                                    entry("demo-c", "1.0.0", {"demo-b"})},
                                   {"demo-a"});

    CHECK(graph.unmet_text("demo-a").empty());  // 它自己没缺东西，是用户停的
    CHECK(graph.unmet_text("demo-b").find("demo-a") != std::string::npos);
    CHECK(graph.unmet_text("demo-c").find("demo-b") != std::string::npos);

    // 不可用往下传之后，仍然要按依赖顺序来
    const auto plan = compute_plan(graph, {{"demo-a", true}, {"demo-b", true}, {"demo-c", true}});
    REQUIRE(plan.size() == 2);
    CHECK_FALSE(plan[0].activate);
    CHECK(plan[0].plugin_id == "demo-c");  // 反向拓扑序：先停最下游
    CHECK_FALSE(plan[1].activate);
    CHECK(plan[1].plugin_id == "demo-b");
}

TEST_CASE("环检测给出具体环路", "[depgraph]") {
    const auto flat = build_graph({entry("demo-a", "1.0.0"), entry("demo-b", "1.0.0", {"demo-a"})});
    CHECK(find_cycle(flat).empty());

    const auto cyclic = build_graph(
        {entry("demo-a", "1.0.0", {"demo-b"}), entry("demo-b", "1.0.0", {"demo-a"})});
    const auto cycle = find_cycle(cyclic);
    REQUIRE(cycle.size() >= 3);          // 首尾相同
    CHECK(cycle.front() == cycle.back());
    CHECK((cycle.front() == "demo-a" || cycle.front() == "demo-b"));
}

TEST_CASE("拓扑序让被依赖者在前", "[depgraph]") {
    const auto graph = build_graph({entry("demo-a", "1.0.0"), entry("demo-b", "1.0.0", {"demo-a"}),
                                    entry("demo-c", "1.0.0", {"demo-b"})});
    const auto order = topological_order(graph);
    REQUIRE(order.size() == 3);
    CHECK(order[0] == "demo-a");
    CHECK(order[1] == "demo-b");
    CHECK(order[2] == "demo-c");

    // 独立节点也进序，且同层按名字排序，输出稳定
    const auto two = build_graph({entry("x-1", "1.0.0"), entry("x-2", "1.0.0")});
    CHECK(topological_order(two) == std::vector<std::string>{"x-1", "x-2"});
}

TEST_CASE("计划：只动需要动的", "[depgraph]") {
    const auto graph = build_graph({entry("demo-a", "1.0.0"), entry("demo-b", "1.0.0", {"demo-a"}),
                                    entry("demo-c", "1.0.0", {"demo-x"})});

    SECTION("未激活且需求满足 → 激活，按拓扑序") {
        const auto plan = compute_plan(graph, {{"demo-a", false}, {"demo-b", false},
                                               {"demo-c", false}});
        REQUIRE(plan.size() == 2);
        CHECK(plan[0].activate);
        CHECK(plan[0].plugin_id == "demo-a");
        CHECK(plan[1].activate);
        CHECK(plan[1].plugin_id == "demo-b");
    }

    SECTION("已激活但需求不满足 → 停用") {
        const auto plan = compute_plan(graph, {{"demo-a", true}, {"demo-b", true},
                                              {"demo-c", true}});
        REQUIRE(plan.size() == 1);
        CHECK_FALSE(plan[0].activate);
        CHECK(plan[0].plugin_id == "demo-c");
    }

    SECTION("全都对得上 → 空计划") {
        const auto plan = compute_plan(graph, {{"demo-a", true}, {"demo-b", true},
                                               {"demo-c", false}});
        CHECK(plan.empty());
    }

    SECTION("先停后起：停用在激活之前") {
        const auto graph2 = build_graph({entry("demo-a", "1.0.0"),
                                         entry("demo-c", "1.0.0", {"demo-x"})});
        const auto plan = compute_plan(graph2, {{"demo-a", false}, {"demo-c", true}});
        REQUIRE(plan.size() == 2);
        CHECK_FALSE(plan[0].activate);
        CHECK(plan[0].plugin_id == "demo-c");
        CHECK(plan[1].activate);
        CHECK(plan[1].plugin_id == "demo-a");
    }

    SECTION("依赖只看装没装，不看启没启") {
        // 04/04 §5.1 的三态判定里，「需求满足」指目标已安装且版本匹配。
        // demo-a 被停用不影响 demo-b 的判定，所以这里只把 demo-a 起来。
        const auto graph2 = build_graph({entry("demo-a", "1.0.0"),
                                         entry("demo-b", "1.0.0", {"demo-a"})});
        const auto plan = compute_plan(graph2, {{"demo-a", false}, {"demo-b", true}});
        REQUIRE(plan.size() == 1);
        CHECK(plan[0].activate);
        CHECK(plan[0].plugin_id == "demo-a");
    }
}

TEST_CASE("有环时不给计划", "[depgraph]") {
    const auto graph = build_graph(
        {entry("demo-a", "1.0.0", {"demo-b"}), entry("demo-b", "1.0.0", {"demo-a"})});
    CHECK(compute_plan(graph, {{"demo-a", false}, {"demo-b", false}}).empty());
}
