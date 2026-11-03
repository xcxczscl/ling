/* 外壳：启动、侧边栏、设置二级界面、通用水滴（提示条与确认框）。
 *
 * 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/02-外壳页面与布局.md。
 * 页面是单页应用：所有切换在同一文档内完成，刷新会丢掉已打开面板的状态。
 */
(function () {
  "use strict";

  const L = (window.Ling = window.Ling || {});

  const STORAGE_TOKEN = "ling-ui-token";
  const HOME_HINT = "请从 ling 主程序打开界面";
  // 宿主的管理动作最长等 30 秒（安装要解压）；客户端多留一点余量。
  const API_TIMEOUT_MS = 40000;

  const state = {
    panels: [],
    tabs: [],
    activeTab: null,
    hostDown: false,
    settingsOpen: false
  };

  // 六个页签各自一个文件，加载时把自己注册进来；外壳只在切到该页签时挂载它。
  L.viewModules = L.viewModules || {};
  L.registerView = (name, definition) => {
    L.viewModules[name] = definition;
  };

  // 页签公用的小工具：建元素时只用 textContent，不拼 HTML 字符串。
  L.dom = {
    el(tag, className, text) {
      const node = document.createElement(tag);
      if (className) {
        node.className = className;
      }
      if (text !== undefined && text !== null) {
        node.textContent = text;
      }
      return node;
    },
    button(text, onClick, className) {
      const node = document.createElement("button");
      node.type = "button";
      node.textContent = text;
      if (className) {
        node.className = className;
      }
      node.addEventListener("click", onClick);
      return node;
    }
  };

  // ---------------- 通信用 ----------------

  function storedToken() {
    try {
      return sessionStorage.getItem(STORAGE_TOKEN) || "";
    } catch (error) {
      return "";  // 无痕模式下 sessionStorage 会抛，退化成「没有口令」
    }
  }

  function rememberToken(token) {
    try {
      sessionStorage.setItem(STORAGE_TOKEN, token);
    } catch (error) {
      // 存不下就只在本次页面里用，刷新后要重新从主程序打开。
    }
  }

  function tokenFromUrl() {
    const query = location.search;
    if (!query || query.length < 2) {
      return "";
    }
    for (const piece of query.slice(1).split("&")) {
      const eq = piece.indexOf("=");
      if (eq > 0 && piece.slice(0, eq) === "token") {
        return decodeURIComponent(piece.slice(eq + 1));
      }
    }
    return "";
  }

  function errorOf(error) {
    if (error && typeof error === "object" && error.code) {
      return error;
    }
    return { code: "E_INTERNAL", message: (error && error.message) || "请求失败" };
  }

  async function api(action, args) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), API_TIMEOUT_MS);
    try {
      const response = await fetch("/api/" + action, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: "Bearer " + storedToken()
        },
        body: JSON.stringify(args || {}),
        signal: controller.signal
      });
      if (response.status === 401) {
        fail401();
        throw { code: "E_AUTH", message: "口令已变，请重新从主程序打开" };
      }
      let body = null;
      try {
        body = await response.json();
      } catch (error) {
        throw {
          code: "E_PROTOCOL",
          message: "ling 返回了看不懂的内容（HTTP " + response.status + "）"
        };
      }
      if (!body || body.ok !== true) {
        const error = (body && body.error) || {
          code: "E_INTERNAL",
          message: "请求失败（HTTP " + response.status + "）"
        };
        throw error;
      }
      return body.result;
    } catch (error) {
      if (error && error.name === "AbortError") {
        throw { code: "E_TIMEOUT", message: "ling 没有响应，正在忙或已卡住" };
      }
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  function fail401() {
    document.body.innerHTML = "";
    const box = document.createElement("div");
    box.style.padding = "48px";
    box.style.textAlign = "center";
    const line = document.createElement("p");
    line.textContent = "口令已变，请重新从主程序打开";
    box.appendChild(line);
    document.body.appendChild(box);
  }

  // ---------------- 提示条与确认框 ----------------

  function toast(text, kind) {
    const host = document.getElementById("toast-host");
    const box = document.createElement("div");
    box.className = "toast" + (kind === "danger" ? " danger" : "");
    const line = document.createElement("div");
    line.textContent = text;
    box.appendChild(line);
    host.appendChild(box);
    setTimeout(() => box.remove(), kind === "danger" ? 6000 : 2500);
  }

  function toastError(error) {
    const info = errorOf(error);
    const host = document.getElementById("toast-host");
    const box = document.createElement("div");
    box.className = "toast danger";
    const line = document.createElement("div");
    line.textContent = info.message || info.code;
    box.appendChild(line);
    // advice 有就显示，没有就不留空行（"advice" in error 判断，不看真值）。
    if ("advice" in info && info.advice) {
      const advice = document.createElement("div");
      advice.className = "advice";
      advice.textContent = info.advice;
      box.appendChild(advice);
    }
    host.appendChild(box);
    setTimeout(() => box.remove(), 6000);
  }

  let dialogResolver = null;

  function confirmDialog(options) {
    if (dialogResolver) {
      // 新框顶掉旧框：旧 Promise 先按「取消」收尾，否则等它的人会永远挂住。
      const previous = dialogResolver;
      dialogResolver = null;
      previous(false);
    }
    const host = document.getElementById("dialog-host");
    host.innerHTML = "";
    host.hidden = false;

    const box = document.createElement("div");
    box.className = "dialog";
    const title = document.createElement("h2");
    title.textContent = options.title || "确认";
    const text = document.createElement("p");
    text.textContent = options.text || "";
    const actions = document.createElement("div");
    actions.className = "actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = "取消";
    const okButton = document.createElement("button");
    okButton.type = "button";
    okButton.className = options.danger ? "danger" : "primary";
    okButton.textContent = options.okText || "确定";
    actions.appendChild(cancel);
    actions.appendChild(okButton);
    box.appendChild(title);
    box.appendChild(text);
    box.appendChild(actions);
    host.appendChild(box);
    okButton.focus();

    return new Promise((resolve) => {
      const settle = (value) => {
        host.hidden = true;
        host.innerHTML = "";
        dialogResolver = null;
        resolve(value);
      };
      dialogResolver = settle;
      cancel.addEventListener("click", () => settle(false));
      okButton.addEventListener("click", () => settle(true));
    });
  }

  // ---------------- 侧边栏 ----------------

  function initialOf(title) {
    return title && title.length > 0 ? title.slice(0, 1) : "?";
  }

  function renderSidebar() {
    const nav = document.getElementById("nav-views");
    nav.innerHTML = "";
    for (const panel of state.panels) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "view-item";
      button.dataset.plugin = panel.id;
      if (panel.state !== "running") {
        button.classList.add("off");
      }
      if (L.panels && L.panels.isOpen(panel.id)) {
        button.classList.add("active");
      }
      if (panel.icon) {
        const img = document.createElement("img");
        img.src = panel.icon;
        img.alt = "";
        button.appendChild(img);
      } else {
        const initial = document.createElement("span");
        initial.className = "initial";
        initial.textContent = initialOf(panel.title);
        button.appendChild(initial);
      }
      const label = document.createElement("span");
      label.className = "label";
      label.textContent = panel.title;
      button.appendChild(label);
      button.title = panel.title;
      button.addEventListener("click", () => {
        closeSettings();
        L.panels.open(panel.id, panel);
      });
      nav.appendChild(button);
    }
    renderEmptyState();
  }

  function renderEmptyState() {
    const empty = document.getElementById("empty-state");
    if (state.panels.length === 0) {
      empty.textContent = "当前没有带界面的插件。可以在设置 → 插件里安装。";
      empty.hidden = false;
      return;
    }
    const opened = L.panels ? L.panels.opened().length : 0;
    if (opened === 0 && !state.settingsOpen) {
      empty.textContent = "从左侧选一个插件开始。";
      empty.hidden = false;
      return;
    }
    empty.hidden = true;
  }

  function markActive() {
    for (const button of document.querySelectorAll(".view-item")) {
      button.classList.toggle(
        "active",
        L.panels && L.panels.isOpen(button.dataset.plugin)
      );
    }
  }

  // ---------------- 宿主状态与面板列表 ----------------

  let failures = 0;
  let refreshing = false;

  async function refreshPanels() {
    if (refreshing) {
      return;  // 上一轮还没回来，别叠着发
    }
    refreshing = true;
    try {
      const list = await api("panels");
      failures = 0;
      setHostState(true);
      // 只有真的变了才重画侧边栏，否则每 2 秒会重建一遍 DOM。
      if (JSON.stringify(list) !== JSON.stringify(state.panels)) {
        const previous = state.panels;
        state.panels = list;
        // 状态变化：运行中变停用要关面板；卸载则是整条从列表里消失。
        for (const panel of previous) {
          const now = list.find((item) => item.id === panel.id);
          if (!now) {
            L.panels.close(panel.id, "uninstalled");
            toast("插件已卸载：" + panel.title, "info");
          } else if (panel.state === "running" && now.state !== "running") {
            L.panels.close(now.id, "disabled");
            toast("插件已停用：" + now.title, "info");
          }
        }
        renderSidebar();
        markActive();
      }
    } catch (error) {
      failures += 1;
      if (failures >= 2) {
        setHostState(false);
      }
    } finally {
      refreshing = false;
    }
  }

  function setHostState(up) {
    state.hostDown = !up;
    const box = document.getElementById("host-state");
    const text = document.getElementById("host-state-text");
    box.classList.toggle("down", !up);
    text.textContent = up ? "宿主运行中" : "ling 没有响应";
  }

  // ---------------- 设置二级界面 ----------------

  const TABS = [
    { name: "plugins", title: "插件" },
    { name: "logs", title: "日志" },
    { name: "config", title: "配置" },
    { name: "jobs", title: "任务" },
    { name: "appearance", title: "外观" },
    { name: "about", title: "关于" }
  ];

  let mounted = null;

  function renderTabs() {
    const nav = document.getElementById("settings-tabs");
    nav.innerHTML = "";
    for (const tab of TABS) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "tab-item" + (tab.name === state.activeTab ? " active" : "");
      button.textContent = tab.title;
      button.addEventListener("click", () => showTab(tab.name));
      nav.appendChild(button);
    }
  }

  function showTab(name) {
    const known = TABS.find((tab) => tab.name === name) ? name : "plugins";
    state.activeTab = known;
    history.replaceState({}, "", "#settings/" + known);
    renderTabs();
    const body = document.getElementById("settings-body");
    if (mounted && typeof mounted.unmount === "function") {
      mounted.unmount();
    }
    mounted = null;
    body.innerHTML = "";
    const module = L.viewModules[known];
    if (!module) {
      body.textContent = "这个页签还没加载";
      return;
    }
    mounted = module.mount(body) || null;
  }

  function openSettings(tab) {
    state.settingsOpen = true;
    document.getElementById("settings").hidden = false;
    showTab(tab || state.activeTab || "plugins");
    renderEmptyState();
  }

  function closeSettings() {
    if (!state.settingsOpen) {
      return;
    }
    state.settingsOpen = false;
    document.getElementById("settings").hidden = true;
    if (history.hash) {
      history.replaceState({}, "", location.pathname);
    }
    if (mounted && typeof mounted.unmount === "function") {
      mounted.unmount();
    }
    mounted = null;
    renderEmptyState();
  }

  // ---------------- 侧边栏收缩 ----------------

  async function applySidebar(collapsed, persist) {
    document.getElementById("app").classList.toggle("sidebar-collapsed", collapsed);
    if (!persist) {
      return;
    }
    try {
      await api("config.set", { key: "core-ui.sidebar", value: !collapsed });
    } catch (error) {
      // 记住与否不影响本次使用。
    }
  }

  async function loadSidebar() {
    let collapsed = window.innerWidth < 900;
    try {
      const result = await api("config.get", { key: "core-ui.sidebar" });
      if (typeof result.value === "boolean") {
        collapsed = !result.value;
      } else {
        const fallback = await api("config.get", { key: "core-ui.sidebar_default" });
        if (typeof fallback.value === "boolean") {
          collapsed = !fallback.value;
        }
      }
    } catch (error) {
      // 读不到就按窗口宽度决定。
    }
    applySidebar(collapsed, false);
  }

  // ---------------- 键盘 ----------------

  function installKeyboard() {
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        const host = document.getElementById("dialog-host");
        if (!host.hidden) {
          host.hidden = true;
          host.innerHTML = "";
          if (dialogResolver) {
            dialogResolver(false);
            dialogResolver = null;
          }
          return;
        }
        closeSettings();
        return;
      }
      if (event.altKey && event.key >= "1" && event.key <= "4") {
        const index = Number(event.key) - 1;
        const opened = L.panels ? L.panels.opened() : [];
        const target = document.querySelectorAll("#panel-area .panel")[index];
        if (opened[index] && target) {
          event.preventDefault();
          target.setAttribute("tabindex", "-1");
          target.focus();
        }
      }
    });
  }

  // ---------------- 启动 ----------------

  async function boot() {
    // 口令只经 URL 进来一次：存进 sessionStorage，然后立刻把地址栏抹掉。
    const fromUrl = tokenFromUrl();
    if (fromUrl) {
      rememberToken(fromUrl);
      history.replaceState({}, "", "/");
    }
    if (!storedToken()) {
      document.body.innerHTML = "";
      const box = document.createElement("div");
      box.style.padding = "48px";
      box.style.textAlign = "center";
      const line = document.createElement("p");
      line.textContent = HOME_HINT;
      box.appendChild(line);
      document.body.appendChild(box);
      return;
    }

    L.toast = toast;
    L.toastError = toastError;
    L.api = api;
    L.errorOf = errorOf;
    L.confirm = confirmDialog;
    L.panelInfo = (id) => state.panels.find((item) => item.id === id) || null;
    L.refreshPanels = refreshPanels;
    L.openSettings = openSettings;
    L.enablePlugin = async (id) => {
      await api("enable", { id });
      await refreshPanels();
    };

    try {
      await L.theme.load();
    } catch (error) {
      L.theme.apply("light");
    }

    L.panels.init({
      onChange: () => {
        markActive();
        renderEmptyState();
      },
      log: (message) => console.warn("[core-ui] " + message)
    });

    await loadSidebar();
    await refreshPanels();
    renderSidebar();

    // 面板列表 2 秒刷一次：别处改的插件状态最迟 2 秒后反映到界面上。
    setInterval(refreshPanels, 2000);

    await L.panels.restore(state.panels);
    markActive();
    renderEmptyState();

    document.getElementById("btn-settings").addEventListener("click", () => openSettings());
    document.getElementById("btn-settings-close").addEventListener("click", closeSettings);
    document
      .getElementById("btn-collapse")
      .addEventListener("click", () =>
        applySidebar(
          !document.getElementById("app").classList.contains("sidebar-collapsed"),
          true
        )
      );
    installKeyboard();

    if (location.hash.indexOf("#settings") === 0) {
      const parts = location.hash.split("/");
      openSettings(parts[1] || "plugins");
    }
  }

  document.addEventListener("DOMContentLoaded", boot);
})();
