// 清单校验的单元测试。判据来自
// docs/ling/04-详细设计/02-插件装载与接口.md §3.1 至 §3.4，
// 验收标准来自 docs/ling/01-需求规格.md 的 001 至 006。
#include <catch_amalgamated.hpp>

#include <filesystem>
#include <fstream>

#include "ling/errors.hpp"
#include "ling/manifest.hpp"

namespace {

// 造一个临时插件目录，写入清单与产物，返回目录路径。
// 目录名必须等于清单 id（校验第 1 步），所以外面再套一层随机根目录避免互相干扰。
std::filesystem::path make_plugin_dir(const std::string& id, const std::string& manifest_body,
                                      bool with_artifact = true) {
    const auto root = std::filesystem::temp_directory_path() / ("ling-test-root-" + id);
    const auto dir = root / id;
    std::filesystem::remove_all(root);
    std::filesystem::create_directories(dir);

    std::ofstream manifest(dir / "plugin.toml");
    manifest << manifest_body;
    manifest.close();

    if (with_artifact) {
        std::ofstream artifact(dir / (id + ".dll"));
        artifact << "stub";
    }
    return dir;
}

std::string base_manifest(const std::string& id) {
    return "id = \"" + id +
           "\"\nversion = \"1.0.0\"\napi = \"1\"\nname = \"测试插件\"\n";
}

}  // namespace

TEST_CASE("插件编号格式", "[manifest]") {
    CHECK(ling::valid_plugin_id("demo-a"));
    CHECK(ling::valid_plugin_id("ab"));
    CHECK_FALSE(ling::valid_plugin_id("Demo"));
    CHECK_FALSE(ling::valid_plugin_id("demo_a"));
    CHECK_FALSE(ling::valid_plugin_id("-demo"));
    CHECK_FALSE(ling::valid_plugin_id(std::string(65, 'a')));
}

TEST_CASE("版本号三段式与范围", "[manifest]") {
    CHECK(ling::valid_version("1.0.0"));
    CHECK(ling::valid_version("0.0.1"));
    CHECK(ling::valid_version("65535.65535.65535"));
    CHECK_FALSE(ling::valid_version("1.0"));
    CHECK_FALSE(ling::valid_version("1.0.0.0"));
    CHECK_FALSE(ling::valid_version("70000.0.0"));
}

TEST_CASE("min_host 比较", "[manifest]") {
    CHECK(ling::version_at_least("0.0.1", "0.0.1"));
    CHECK(ling::version_at_least("0.1.0", "0.0.9"));
    CHECK_FALSE(ling::version_at_least("0.0.1", "0.0.2"));
    CHECK(ling::version_at_least("0.0.1", ""));
}

TEST_CASE("清单完整时通过校验", "[manifest]") {
    const auto dir = make_plugin_dir("demo-ok", base_manifest("demo-ok"));
    const auto result = ling::load_manifest(dir, "0.0.1");
    INFO("code=" << result.code << " detail=" << result.detail << " dir=" << dir.string());
    REQUIRE(result.ok());
    CHECK(result.manifest->id == "demo-ok");
    CHECK(result.manifest->artifact == "demo-ok.dll");
    std::filesystem::remove_all(dir.parent_path());
}

TEST_CASE("缺必填字段被拒且指出是哪一项", "[manifest]") {
    const auto dir = make_plugin_dir("demo-miss", "id = \"demo-miss\"\nversion = \"1.0.0\"\n");
    const auto result = ling::load_manifest(dir, "0.0.1");
    REQUIRE_FALSE(result.ok());
    CHECK(result.code == ling::err::kManifestInvalid);
    CHECK(result.detail.find("api") != std::string::npos);
    std::filesystem::remove_all(dir.parent_path());
}

TEST_CASE("目录名与 id 不一致被拒", "[manifest]") {
    const auto dir = make_plugin_dir("demo-dir", base_manifest("demo-other"));
    const auto result = ling::load_manifest(dir, "0.0.1");
    REQUIRE_FALSE(result.ok());
    CHECK(result.code == ling::err::kManifestInvalid);
    std::filesystem::remove_all(dir.parent_path());
}

TEST_CASE("api 主版本不支持时返回 E_API_MISMATCH", "[manifest]") {
    // base_manifest 已含 api = "1"，这里单独拼一份，避免重复键让 toml++ 直接报解析失败。
    const std::string body =
        "id = \"demo-api\"\nversion = \"1.0.0\"\napi = \"9\"\nname = \"测试插件\"\n";
    const auto dir = make_plugin_dir("demo-api", body);
    const auto result = ling::load_manifest(dir, "0.0.1");
    REQUIRE_FALSE(result.ok());
    CHECK(result.code == ling::err::kApiMismatch);
    std::filesystem::remove_all(dir.parent_path());
}

TEST_CASE("缺少产物被拒", "[manifest]") {
    const auto dir = make_plugin_dir("demo-noart", base_manifest("demo-noart"), false);
    const auto result = ling::load_manifest(dir, "0.0.1");
    INFO("code=" << result.code << " detail=" << result.detail);
    REQUIRE_FALSE(result.ok());
    CHECK(result.detail.find("缺少产物") != std::string::npos);
    std::filesystem::remove_all(dir.parent_path());
}

TEST_CASE("依赖声明接受，写法错要拦住，侧车仍拒绝", "[manifest]") {
    // 依赖声明随 M3 放开（04/04）；写错的要在这里拦住，否则它会永远不满足，
    // 用户只看到「装不上」却不知道错在哪。
    auto with_deps = base_manifest("demo-deps");
    with_deps += "[requires]\nplugins = [\"core-ui@1\"]\n";
    const auto dir1 = make_plugin_dir("demo-deps", with_deps);
    const auto r1 = ling::load_manifest(dir1, "0.0.1");
    REQUIRE(r1.ok());
    REQUIRE(r1.manifest->requires_plugins.size() == 1);
    CHECK(r1.manifest->requires_plugins[0] == "core-ui@1");
    std::filesystem::remove_all(dir1);

    auto bad_deps = base_manifest("demo-baddeps");
    bad_deps += "[requires]\nplugins = [\"core-ui@1.2.3.4\"]\n";
    const auto dir_bad = make_plugin_dir("demo-baddeps", bad_deps);
    const auto r_bad = ling::load_manifest(dir_bad, "0.0.1");
    REQUIRE_FALSE(r_bad.ok());
    CHECK(r_bad.detail.find("依赖写法不对") != std::string::npos);
    std::filesystem::remove_all(dir_bad);

    auto with_sidecar = base_manifest("demo-side");
    with_sidecar += "[sidecar]\nexecutable = \"sidecar/run.exe\"\n";
    const auto dir2 = make_plugin_dir("demo-side", with_sidecar, false);
    const auto r2 = ling::load_manifest(dir2, "0.0.1");
    REQUIRE_FALSE(r2.ok());
    CHECK(r2.detail.find("侧车") != std::string::npos);
    std::filesystem::remove_all(dir2);
}
