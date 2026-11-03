/* 设置 → 外观页（04-详细设计/02 §4.6）。
 *
 * 两张预览卡用的是令牌自身的颜色，所以「切到暗色时两张卡都跟着变」——
 * 用户看到的是真实效果，不是图片。
 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  L.registerView("appearance", {
    mount(root) {
      let sidebarDefault = true;

      const title = el("h2", null, "主题");
      title.style.fontSize = "var(--ling-fs-lg)";
      root.appendChild(title);

      const cards = el("div", "theme-cards");
      for (const theme of ["light", "dark"]) {
        const card = el("button", "theme-card" + (L.theme.get() === theme ? " active" : ""));
        const preview = el("div", "preview");
        preview.appendChild(el("div", "bar"));
        preview.appendChild(el("div", "line"));
        preview.appendChild(el("div", "line short"));
        card.appendChild(preview);
        card.appendChild(el("div", "caption", theme === "dark" ? "暗色" : "亮色"));
        card.addEventListener("click", async () => {
          try {
            await L.theme.set(theme);
          } catch (error) {
            L.toastError(error);
          }
          for (const node of cards.querySelectorAll(".theme-card")) {
            node.classList.toggle("active", node === card);
          }
        });
        cards.appendChild(card);
      }
      root.appendChild(cards);

      const gap = el("div");
      gap.style.height = "var(--ling-sp-5)";
      root.appendChild(gap);

      const head = el("h2", null, "侧边栏");
      head.style.fontSize = "var(--ling-fs-lg)";
      root.appendChild(head);

      const row = el("div", "row");
      row.appendChild(el("label", null, "打开界面时展开"));
      const control = el("div", "control");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = !document.getElementById("app").classList.contains("sidebar-collapsed");
      control.appendChild(box);
      row.appendChild(control);
      root.appendChild(row);

      const note = el(
        "div",
        "small dim",
        "这里只管「打开时」的初始状态；当前收没收起另存在 core-ui.sidebar，两个键不混用。"
      );
      root.appendChild(note);

      box.addEventListener("change", async () => {
        sidebarDefault = box.checked;
        try {
          await L.api("config.set", { key: "core-ui.sidebar_default", value: sidebarDefault });
          L.toast("已保存");
        } catch (error) {
          box.checked = !sidebarDefault;
          L.toastError(error);
        }
      });

      L.api("config.get", { key: "core-ui.sidebar_default" })
        .then((result) => {
          if (typeof result.value === "boolean") {
            box.checked = result.value;
          }
        })
        .catch(() => {});

      return { unmount() {} };
    }
  });
})();
