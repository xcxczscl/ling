/* 设置 → 插件页。行为沿用 1.0.0 的插件视图（04-详细设计/02 §4.2）。 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  // 内核的四个状态值到三个中文词只在这一处换算。
  const STATE_TEXT = {
    activated: { text: "已启用", tone: "ok", filter: "running" },
    deactivated: { text: "已停用", tone: "", filter: "stopped" },
    failed: { text: "启动失败", tone: "danger", filter: "failed" },
    crashed: { text: "启动失败", tone: "danger", filter: "failed" }
  };

  const TRACK_TEXT = { process: "进程内", sidecar: "独立进程" };

  function stateOf(plugin) {
    return STATE_TEXT[plugin.state] || { text: plugin.state, tone: "", filter: "other" };
  }

  L.registerView("plugins", {
    mount(root) {
      let plugins = [];
      let withUi = new Set();
      let filter = "all";
      let keyword = "";
      let busy = false;
      let expanded = null;

      const toolbar = el("div", "toolbar");
      const installInput = document.createElement("input");
      installInput.type = "text";
      installInput.placeholder = "插件目录的绝对路径";
      installInput.size = 36;
      const installButton = el("button", "primary", "安装");
      const pasteButton = el("button", null, "从剪贴板读取");
      const filterSelect = document.createElement("select");
      for (const [value, text] of [
        ["all", "全部"],
        ["running", "在用"],
        ["stopped", "停用"],
        ["failed", "失败"]
      ]) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = text;
        filterSelect.appendChild(option);
      }
      const search = document.createElement("input");
      search.type = "search";
      search.placeholder = "按名筛选";
      const spacer = el("div", "spacer");

      toolbar.appendChild(installInput);
      toolbar.appendChild(installButton);
      toolbar.appendChild(pasteButton);
      toolbar.appendChild(spacer);
      toolbar.appendChild(filterSelect);
      toolbar.appendChild(search);
      root.appendChild(toolbar);

      const notice = el("div", "notice");
      notice.hidden = true;
      root.appendChild(notice);

      const table = el("table", "grid");
      const thead = el("thead");
      const headRow = el("tr");
      for (const title of ["插件名", "版本", "状态", "类型", "界面", "操作"]) {
        headRow.appendChild(el("th", null, title));
      }
      thead.appendChild(headRow);
      table.appendChild(thead);
      const tbody = el("tbody");
      table.appendChild(tbody);
      root.appendChild(table);

      const empty = el("div", "empty", "没有符合条件的插件");
      empty.hidden = true;
      root.appendChild(empty);

      async function reload() {
        try {
          plugins = await L.api("plugins");
          // 「有没有界面」不在 kernel.plugins 里，拿面板表交叉一下。
          const panels = await L.api("panels").catch(() => []);
          withUi = new Set(panels.map((item) => item.id));
        } catch (error) {
          L.toastError(error);
          return;
        }
        render();
      }

      function visible() {
        return plugins.filter((plugin) => {
          const state = stateOf(plugin);
          if (filter !== "all" && state.filter !== filter) {
            return false;
          }
          if (keyword && plugin.id.indexOf(keyword) < 0) {
            return false;
          }
          return true;
        });
      }

      function render() {
        tbody.innerHTML = "";
        const list = visible();
        empty.hidden = list.length > 0;
        for (const plugin of list) {
          const state = stateOf(plugin);
          const row = el("tr");
          if (plugin.state !== "activated") {
            row.classList.add("expandable");
          }
          row.appendChild(el("td", "mono", plugin.id));
          row.appendChild(el("td", "dim small", plugin.version || ""));
          const stateCell = el("td");
          const tag = el("span", "tag " + state.tone, state.text);
          stateCell.appendChild(tag);
          row.appendChild(stateCell);
          row.appendChild(el("td", null, TRACK_TEXT[plugin.track] || plugin.track));
          const uiCell = el("td");
          if (withUi.has(plugin.id)) {
            uiCell.appendChild(el("span", "tag", "有界面"));
          } else {
            uiCell.textContent = "—";
            uiCell.className = "dim";
          }
          row.appendChild(uiCell);

          const actions = el("td");
          const enabled = plugin.state === "activated";
          const toggle = el("button", null, enabled ? "停用" : "启用");
          toggle.disabled = busy;
          toggle.addEventListener("click", (event) => {
            event.stopPropagation();
            togglePlugin(plugin, enabled);
          });
          actions.appendChild(toggle);
          const remove = el("button", null, "卸载");
          remove.className = "ghost";
          remove.disabled = busy;
          remove.addEventListener("click", (event) => {
            event.stopPropagation();
            uninstall(plugin, false);
          });
          actions.appendChild(remove);
          row.appendChild(actions);

          row.addEventListener("click", () => {
            if (plugin.state === "activated") {
              return;
            }
            expanded = expanded === plugin.id ? null : plugin.id;
            render();
          });
          tbody.appendChild(row);

          if (expanded === plugin.id) {
            const detail = el("tr", "detail-row");
            const cell = document.createElement("td");
            cell.colSpan = 6;
            cell.textContent = plugin.reason || "没有更多信息";
            cell.appendChild(document.createElement("br"));
            cell.appendChild(
              L.dom.button("看日志", () => {
                window.__lingLogPlugin = plugin.id;
                L.openSettings("logs");
              })
            );
            cell.appendChild(document.createTextNode(" "));
            const purge = L.dom.button("卸载并删除数据", () => uninstall(plugin, true), "danger");
            purge.disabled = busy;
            cell.appendChild(purge);
            detail.appendChild(cell);
            tbody.appendChild(detail);
          }
        }
      }

      async function togglePlugin(plugin, wasEnabled) {
        const plan = await L.api("plan").catch(() => null);
        let text = wasEnabled
          ? "停用 " + plugin.id + "？它会停止工作，数据保留。"
          : "启用 " + plugin.id + "？";
        if (wasEnabled && plan && Array.isArray(plan.steps)) {
          const dependents = plan.steps
            .filter((step) => step.plugin !== plugin.id && step.action === "disable")
            .map((step) => step.plugin);
          if (dependents.length > 0) {
            text += "同时会停用 " + dependents.join("、") + "。";
          }
        }
        // 停用要不要确认由 core-ui.confirm_disable 决定，默认要；有下游时始终要。
        if (wasEnabled && text.indexOf("同时会停用") < 0) {
          const setting = await L.api("config.get", { key: "core-ui.confirm_disable" }).catch(
            () => null
          );
          if (setting && setting.value === false) {
            return doToggle(plugin, wasEnabled);
          }
        }
        const yes = await L.confirm({
          title: wasEnabled ? "停用插件" : "启用插件",
          text,
          okText: wasEnabled ? "停用" : "启用",
          danger: wasEnabled
        });
        if (yes) {
          doToggle(plugin, wasEnabled);
        }
      }

      async function doToggle(plugin, wasEnabled) {
        busy = true;
        render();
        try {
          await L.api(wasEnabled ? "disable" : "enable", { id: plugin.id });
          await reload();
        } catch (error) {
          L.toastError(error);
        } finally {
          busy = false;
          render();
        }
      }

      async function uninstall(plugin, purge) {
        const yes = await L.confirm({
          title: purge ? "卸载并删除数据" : "卸载插件",
          text: purge
            ? "卸载 " + plugin.id + " 并删除它的全部数据？这一步不能撤销。"
            : "卸载 " + plugin.id + "？它的数据会留在插件数据目录，以后装回来还能用。",
          okText: purge ? "卸载并删除" : "卸载",
          danger: true
        });
        if (!yes) {
          return;
        }
        busy = true;
        render();
        try {
          await L.api("uninstall", { id: plugin.id, purge: purge });
          L.toast("已卸载 " + plugin.id);
          await reload();
          await L.refreshPanels();
        } catch (error) {
          L.toastError(error);
        } finally {
          busy = false;
          render();
        }
      }

      function showNotice(text, danger) {
        notice.textContent = text;
        notice.className = "notice" + (danger ? " danger" : "");
        notice.hidden = false;
      }

      installButton.addEventListener("click", async () => {
        const path = installInput.value.trim();
        if (!path) {
          showNotice("先填插件目录的绝对路径", true);
          return;
        }
        installButton.disabled = true;
        installButton.textContent = "处理中";
        try {
          const result = await L.api("install", { path });
          L.toast("已安装 " + result.id + " " + (result.version || ""));
          installInput.value = "";
          notice.hidden = true;
          await reload();
          await L.refreshPanels();
        } catch (error) {
          showNotice(L.errorOf(error).message, true);
        } finally {
          installButton.disabled = false;
          installButton.textContent = "安装";
        }
      });

      pasteButton.addEventListener("click", async () => {
        try {
          const text = await navigator.clipboard.readText();
          if (text) {
            installInput.value = text.trim();
          }
        } catch (error) {
          showNotice("浏览器不给读剪贴板，请手动粘贴路径", true);
        }
      });

      filterSelect.addEventListener("change", () => {
        filter = filterSelect.value;
        render();
      });
      search.addEventListener("input", () => {
        keyword = search.value.trim();
        render();
      });

      reload();
      return {
        unmount() {}
      };
    }
  });
})();
