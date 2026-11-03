/* 主题：应用、广播、读令牌。
 *
 * 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/04 §5：
 *   切换是「先改自己的 <html data-theme>，再广播，最后写配置」，
 *   写失败就回滚——界面与配置不一致比慢一点更难理解。
 * 令牌取值只从 CSS 变量读（getComputedStyle），不在 JS 里再抄一份。
 */
(function () {
  "use strict";

  const L = (window.Ling = window.Ling || {});

  // 26 个令牌的名字。init 消息要把它们连同取值一起发给面板。
  const TOKEN_NAMES = [
    "--ling-bg", "--ling-bg-raised", "--ling-bg-sunken",
    "--ling-text", "--ling-text-dim", "--ling-text-invert",
    "--ling-border", "--ling-border-strong",
    "--ling-accent", "--ling-accent-hover", "--ling-accent-fg",
    "--ling-ok", "--ling-warn", "--ling-danger",
    "--ling-fs-sm", "--ling-fs-md", "--ling-fs-lg", "--ling-fs-xl",
    "--ling-sp-1", "--ling-sp-2", "--ling-sp-3", "--ling-sp-4", "--ling-sp-5",
    "--ling-radius", "--ling-radius-sm", "--ling-shadow"
  ];

  let current = "light";

  function tokens() {
    const style = getComputedStyle(document.documentElement);
    const out = {};
    for (const name of TOKEN_NAMES) {
      out[name] = style.getPropertyValue(name).trim();
    }
    return out;
  }

  function apply(theme) {
    current = theme === "dark" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", current);
  }

  // 启动时读配置。键不存在用 light；取值不认识也当 light 并记一条 warn
  // （配置可手改，值不能信）。
  async function load() {
    let theme = "light";
    try {
      const result = await L.api("config.get", { key: "core-ui.theme" });
      const value = result && result.value;
      if (value === "dark") {
        theme = "dark";
      } else if (value !== "light" && value !== null && value !== undefined) {
        console.warn("[core-ui] core-ui.theme 取值不认识，按 light 处理：", value);
      }
    } catch (error) {
      // 读不到就按默认值来，不打断启动。
      console.warn("[core-ui] 读主题失败，按 light 处理：", error);
    }
    apply(theme);
    return theme;
  }

  // 用户点了外观页的预览卡。
  async function set(theme) {
    const previous = current;
    const next = theme === "dark" ? "dark" : "light";
    if (next === previous) {
      return;
    }
    // 乐观更新：等网络回来再变色会有可感知的延迟。
    apply(next);
    broadcast();
    try {
      await L.api("config.set", { key: "core-ui.theme", value: next });
    } catch (error) {
      // 保存失败就回滚，别让界面显示一个下次启动会变的主题。
      apply(previous);
      broadcast();
      L.toast("主题没能保存，重启后会恢复旧设置", "danger");
      throw error;
    }
  }

  function broadcast() {
    if (L.panels && typeof L.panels.broadcastTheme === "function") {
      L.panels.broadcastTheme(current, tokens());
    }
  }

  L.theme = {
    get: () => current,
    tokens,
    load,
    set,
    broadcast
  };
})();
