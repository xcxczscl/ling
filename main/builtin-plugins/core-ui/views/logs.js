/* 设置 → 日志页（04-详细设计/02 §4.3）。
 *
 * 自动刷新默认开、2 秒一次、只拉最后 200 行；滚到顶部再按 before 上翻一页。
 * 清屏只清界面里的行，不动日志文件——文件是排障时的原始证据。
 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  const AUTO_INTERVAL_MS = 2000;
  const PAGE_SIZE = 200;
  const DOM_LIMIT = 5000;

  L.registerView("logs", {
    mount(root) {
      let plugin = window.__lingLogPlugin || "";
      let level = "";
      let lines = [];
      let timer = 0;
      let loading = false;
      let exhausted = false;

      const toolbar = el("div", "toolbar");
      const pluginSelect = document.createElement("select");
      const levelSelect = document.createElement("select");
      for (const [value, text] of [
        ["", "全部级别"],
        ["debug", "debug"],
        ["info", "info"],
        ["warn", "warn"],
        ["error", "error"]
      ]) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = text;
        levelSelect.appendChild(option);
      }
      const autoLabel = el("label", "small");
      const autoBox = document.createElement("input");
      autoBox.type = "checkbox";
      autoBox.checked = true;
      autoLabel.appendChild(autoBox);
      autoLabel.appendChild(document.createTextNode(" 自动刷新"));
      const refreshButton = el("button", null, "刷新");
      const clearButton = el("button", "ghost", "清屏");

      toolbar.appendChild(pluginSelect);
      toolbar.appendChild(levelSelect);
      toolbar.appendChild(autoLabel);
      toolbar.appendChild(refreshButton);
      toolbar.appendChild(clearButton);
      root.appendChild(toolbar);

      const view = el("div", "log-view");
      root.appendChild(view);
      const empty = el("div", "empty");
      empty.hidden = true;
      root.appendChild(empty);

      async function loadPlugins() {
        try {
          const list = await L.api("plugins");
          pluginSelect.innerHTML = "";
          const none = document.createElement("option");
          none.value = "";
          none.textContent = "选一个插件";
          pluginSelect.appendChild(none);
          for (const item of list) {
            const option = document.createElement("option");
            option.value = item.id;
            option.textContent = item.id;
            pluginSelect.appendChild(option);
          }
          pluginSelect.value = plugin;
        } catch (error) {
          L.toastError(error);
        }
      }

      function lineNode(item) {
        const row = el("div", "log-line");
        row.appendChild(el("span", "ts", item.ts || ""));
        const tag = el("span", "lv " + (item.level || ""), item.level || "");
        row.appendChild(tag);
        // 日志行里可能夹着 <img onerror=…> 这样的东西，一律 textContent。
        row.appendChild(el("span", "text", item.text || ""));
        return row;
      }

      function renderAll() {
        view.innerHTML = "";
        for (const item of lines) {
          view.appendChild(lineNode(item));
        }
        empty.hidden = lines.length > 0 || !plugin;
        if (lines.length === 0 && plugin) {
          empty.textContent = "这个插件还没写过日志";
        }
        view.scrollTop = view.scrollHeight;
      }

      async function fetchLines(options) {
        if (!plugin || loading) {
          return;
        }
        loading = true;
        const asked = { plugin, lines: PAGE_SIZE };
        if (level) {
          asked.level = level;
        }
        if (options && options.before) {
          asked.before = lines.length;
        }
        try {
          const result = await L.api("logs", asked);
          const fresh = result.lines || [];
          if (options && options.before) {
            // 上翻：新的一批插到最前面，滚动条位置保持不动。
            const height = view.scrollHeight;
            lines = fresh.concat(lines);
            renderAll();
            view.scrollTop = view.scrollHeight - height;
          } else {
            lines = fresh;
            renderAll();
          }
          if (lines.length > DOM_LIMIT) {
            lines = lines.slice(lines.length - DOM_LIMIT);
            renderAll();
          }
          exhausted = fresh.length < PAGE_SIZE;
        } catch (error) {
          empty.hidden = false;
          empty.textContent = L.errorOf(error).message;
        } finally {
          loading = false;
        }
      }

      function restart() {
        clearInterval(timer);
        if (autoBox.checked) {
          timer = setInterval(() => fetchLines({}), AUTO_INTERVAL_MS);
        }
      }

      pluginSelect.addEventListener("change", () => {
        plugin = pluginSelect.value;
        lines = [];
        exhausted = false;
        renderAll();
        fetchLines({});
      });
      levelSelect.addEventListener("change", () => {
        level = levelSelect.value;
        lines = [];
        exhausted = false;
        renderAll();
        fetchLines({});
      });
      autoBox.addEventListener("change", restart);
      refreshButton.addEventListener("click", () => fetchLines({}));
      clearButton.addEventListener("click", () => {
        // 清完再刷新会重新拉回来，这是刻意的：界面不当垃圾桶。
        lines = [];
        renderAll();
      });
      view.addEventListener("scroll", () => {
        if (view.scrollTop > 0 || exhausted || lines.length === 0) {
          return;
        }
        fetchLines({ before: true });
      });

      loadPlugins().then(() => {
        if (plugin) {
          renderAll();
          fetchLines({});
        } else {
          empty.hidden = false;
          empty.textContent = "选一个插件看它的日志";
        }
        restart();
      });

      return {
        unmount() {
          clearInterval(timer);
          window.__lingLogPlugin = null;
        }
      };
    }
  });
})();
