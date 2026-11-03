/* 设置 → 关于页（04-详细设计/02 §4.7）。 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  function row(label, value, extra) {
    const wrap = el("div", "row");
    wrap.appendChild(el("label", null, label));
    const control = el("div", "control");
    if (value !== null && value !== undefined) {
      control.appendChild(el("span", "mono", value));
    }
    if (extra) {
      control.appendChild(document.createTextNode(" "));
      control.appendChild(extra);
    }
    wrap.appendChild(control);
    return wrap;
  }

  L.registerView("about", {
    mount(root) {
      let meta = { hostVersion: "", uiVersion: "", dataRoot: "", desktop: false };

      const versionRow = el("div");
      const addressRow = el("div");
      const dataRow = el("div");
      root.appendChild(versionRow);
      root.appendChild(addressRow);
      root.appendChild(dataRow);

      const head = el("h2", null, "关闭窗口时");
      head.style.fontSize = "var(--ling-fs-lg)";
      head.style.marginTop = "var(--ling-sp-5)";
      root.appendChild(head);

      const group = el("div");
      for (const [value, text] of [
        ["keep-host", "只关界面，ling 继续在后台跑"],
        ["stop-host", "关掉界面也退出 ling"]
      ]) {
        const label = el("label", "row");
        const box = document.createElement("input");
        box.type = "radio";
        box.name = "close-behavior";
        box.value = value;
        const control = el("div", "control");
        control.appendChild(box);
        control.appendChild(document.createTextNode(" " + text));
        label.appendChild(el("label", null, ""));
        label.appendChild(control);
        group.appendChild(label);
      }
      root.appendChild(group);
      const note = el("div", "small dim");
      root.appendChild(note);

      const quit = el("button", "danger", "退出 ling");
      const quitRow = el("div", "row");
      quitRow.style.marginTop = "var(--ling-sp-5)";
      quitRow.appendChild(quit);
      root.appendChild(quitRow);

      const details = el("details");
      details.style.marginTop = "var(--ling-sp-5)";
      details.appendChild(el("summary", null, "设计令牌"));
      const tokenTable = el("table", "grid token-table");
      details.appendChild(tokenTable);
      root.appendChild(details);

      function renderTokens() {
        tokenTable.innerHTML = "";
        const tokens = L.theme.tokens();
        const tbody = el("tbody");
        for (const [name, value] of Object.entries(tokens)) {
          const tr = el("tr");
          tr.appendChild(el("td", null, name));
          tr.appendChild(el("td", "dim", value));
          tbody.appendChild(tr);
        }
        tokenTable.appendChild(tbody);
      }

      async function load() {
        try {
          meta = await L.api("meta");
        } catch (error) {
          L.toastError(error);
        }
        versionRow.innerHTML = "";
        versionRow.appendChild(row("宿主版本", meta.hostVersion || ""));
        versionRow.appendChild(row("界面版本", meta.uiVersion || ""));

        const copy = el("button", "ghost", "复制");
        copy.addEventListener("click", async () => {
          try {
            await navigator.clipboard.writeText(location.origin);
            L.toast("已复制");
          } catch (error) {
            L.toast("浏览器不给写剪贴板", "danger");
          }
        });
        addressRow.innerHTML = "";
        addressRow.appendChild(row("界面地址", location.origin, copy));

        dataRow.innerHTML = "";
        dataRow.appendChild(row("数据目录", meta.dataRoot || ""));

        // webui 版「关掉界面」等于关掉标签页，进程拿不到这个事件，选项置灰。
        for (const box of group.querySelectorAll("input[type=radio]")) {
          box.disabled = !meta.desktop;
        }
        note.textContent = meta.desktop
          ? "同一份配置换机器时会一起带走，与其他设置一样可导出。"
          : "浏览器版没有可靠的关窗事件，这一项只在桌面版有效。";
      }

      async function loadBehavior() {
        try {
          const result = await L.api("config.get", { key: "core-ui.close_behavior" });
          const value = typeof result.value === "string" ? result.value : "keep-host";
          for (const box of group.querySelectorAll("input[type=radio]")) {
            box.checked = box.value === value;
          }
        } catch (error) {
          // 读不到就保持默认 keep-host。
        }
      }

      group.addEventListener("change", async (event) => {
        const box = event.target;
        if (!box || box.type !== "radio") {
          return;
        }
        try {
          await L.api("config.set", { key: "core-ui.close_behavior", value: box.value });
          L.toast("已保存");
        } catch (error) {
          L.toastError(error);
        }
      });

      quit.addEventListener("click", async () => {
        const yes = await L.confirm({
          title: "退出 ling",
          text: "关掉界面时同时退出 ling，正在跑的定时任务会中断。确定吗？",
          okText: "退出",
          danger: true
        });
        if (!yes) {
          return;
        }
        try {
          await L.api("shutdown");
        } catch (error) {
          // 宿主退出时可能来不及回包，这不算失败。
        }
      });

      details.addEventListener("toggle", () => {
        if (details.open) {
          renderTokens();
        }
      });

      load();
      loadBehavior();
      return { unmount() {} };
    }
  });
})();
