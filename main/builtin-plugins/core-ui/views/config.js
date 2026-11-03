/* 设置 → 配置页（04-详细设计/02 §4.4）。
 *
 * 左边选插件，右边列键值。改完立即保存，不需要「保存」按钮；
 * 存失败时输入框标红、下面写原因，内容不清空——清空等于让用户重新输一遍。
 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  L.registerView("config", {
    mount(root) {
      let plugins = [];
      let current = "";
      let items = [];

      const layout = el("div");
      layout.style.display = "flex";
      layout.style.gap = "var(--ling-sp-4)";
      layout.style.alignItems = "flex-start";

      const list = el("div");
      list.style.flex = "0 0 160px";
      list.style.display = "flex";
      list.style.flexDirection = "column";
      list.style.gap = "2px";

      const pane = el("div");
      pane.style.flex = "1 1 auto";
      pane.style.minWidth = "0";

      layout.appendChild(list);
      layout.appendChild(pane);
      root.appendChild(layout);

      const hint = el("div", "empty");
      hint.hidden = true;
      pane.appendChild(hint);

      function renderList() {
        list.innerHTML = "";
        for (const plugin of plugins) {
          const button = el(
            "button",
            "tab-item" + (plugin.id === current ? " active" : ""),
            plugin.id
          );
          button.addEventListener("click", () => {
            current = plugin.id;
            renderList();
            loadItems();
          });
          list.appendChild(button);
        }
      }

      async function loadPlugins() {
        try {
          plugins = await L.api("plugins");
        } catch (error) {
          L.toastError(error);
          return;
        }
        if (!plugins.some((plugin) => plugin.id === current)) {
          current = plugins.length > 0 ? plugins[0].id : "";
        }
        renderList();
        if (current) {
          loadItems();
        } else {
          hint.hidden = false;
          hint.textContent = "还没有装任何插件";
        }
      }

      async function loadItems() {
        try {
          const result = await L.api("config.list", { prefix: current + "." });
          items = result.items || [];
        } catch (error) {
          const info = L.errorOf(error);
          pane.innerHTML = "";
          hint.hidden = false;
          hint.textContent = info.message + (info.advice ? " —— " + info.advice : "");
          if (info.code === "E_METHOD_MISSING" || info.code === "E_PLUGIN_MISSING") {
            hint.textContent = "配置功能需要 core-settings，它当前没有启用。";
          }
          return;
        }
        renderItems();
      }

      function control(item, onChange) {
        const wrap = el("div");
        const type = item.type;
        if (type === "bool") {
          const box = document.createElement("input");
          box.type = "checkbox";
          box.checked = item.value === true;
          box.addEventListener("change", () => onChange(box.checked));
          wrap.appendChild(box);
          return wrap;
        }
        if (type === "null") {
          const button = el("button", null, "设置");
          button.addEventListener("click", () => onChange(""));
          wrap.appendChild(el("span", "dim", "（未设置） "));
          wrap.appendChild(button);
          return wrap;
        }
        if (type === "number") {
          const input = document.createElement("input");
          input.type = "number";
          input.value = String(item.value);
          input.addEventListener("change", () => {
            const value = Number(input.value);
            if (Number.isFinite(value)) {
              onChange(value);
            }
          });
          wrap.appendChild(input);
          return wrap;
        }
        if (type === "json") {
          const area = document.createElement("textarea");
          area.value = JSON.stringify(item.value, null, 2);
          area.addEventListener("blur", () => {
            try {
              onChange(JSON.parse(area.value));
              area.classList.remove("invalid");
            } catch (error) {
              area.style.borderColor = "var(--ling-danger)";
              showError(area, "不是合法的 JSON");
            }
          });
          wrap.appendChild(area);
          return wrap;
        }
        const text = typeof item.value === "string" ? item.value : "";
        const multiline = text.length > 80;
        const input = document.createElement(multiline ? "textarea" : "input");
        if (!multiline) {
          input.type = "text";
        }
        input.value = text;
        const commit = () => onChange(input.value);
        input.addEventListener(multiline ? "blur" : "change", commit);
        wrap.appendChild(input);
        return wrap;
      }

      function showError(node, message) {
        const note = el("div", "small", message);
        note.style.color = "var(--ling-danger)";
        node.parentNode.appendChild(note);
      }

      function renderItems() {
        pane.innerHTML = "";
        if (items.length === 0) {
          hint.hidden = false;
          hint.textContent = "这个插件还没有任何配置";
          pane.appendChild(hint);
          return;
        }
        const table = el("table", "grid");
        const tbody = el("tbody");
        for (const item of items) {
          const row = el("tr");
          const keyCell = el("td", "mono", item.key);
          keyCell.style.width = "40%";
          row.appendChild(keyCell);
          const valueCell = el("td");
          valueCell.appendChild(
            control(item, async (value) => {
              try {
                await L.api("config.set", { key: item.key, value });
                item.value = value;
                L.toast("已保存");
              } catch (error) {
                const info = L.errorOf(error);
                valueCell.style.outline = "2px solid var(--ling-danger)";
                showError(valueCell, info.message);
              }
            })
          );
          row.appendChild(valueCell);
          tbody.appendChild(row);
        }
        table.appendChild(tbody);
        pane.appendChild(table);
      }

      loadPlugins();
      return { unmount() {} };
    }
  });
})();
