/* 设置 → 任务页（04-详细设计/02 §4.5）。 */
(function () {
  "use strict";

  const L = window.Ling;
  const el = L.dom.el;

  // 一句人话解释。只覆盖常见写法，其余留空——猜错比不解释更糟。
  function explain(cron) {
    const parts = cron.trim().split(/\s+/);
    if (parts.length !== 5) {
      return "";
    }
    const [minute, hour] = parts;
    if (/^\d+$/.test(minute) && /^\d+$/.test(hour)) {
      return "每天 " + hour.padStart(2, "0") + ":" + minute.padStart(2, "0");
    }
    const step = /^\*\/(\d+)$/.exec(minute);
    if (step) {
      return "每 " + step[1] + " 分钟";
    }
    if (minute === "*") {
      return "每分钟";
    }
    return "";
  }

  L.registerView("jobs", {
    mount(root) {
      let jobs = [];
      let busy = false;

      const toolbar = el("div", "toolbar");
      const addButton = el("button", "primary", "新增");
      const spacer = el("div", "spacer");
      toolbar.appendChild(spacer);
      toolbar.appendChild(addButton);
      root.appendChild(toolbar);

      const table = el("table", "grid");
      const thead = el("thead");
      const headRow = el("tr");
      for (const title of ["名字", "时间表", "目标方法", "上次触发", "操作"]) {
        headRow.appendChild(el("th", null, title));
      }
      thead.appendChild(headRow);
      table.appendChild(thead);
      const tbody = el("tbody");
      table.appendChild(tbody);
      root.appendChild(table);

      const empty = el("div", "empty", "还没有定时任务");
      empty.hidden = true;
      root.appendChild(empty);

      const form = el("div");
      form.hidden = true;
      form.style.marginTop = "var(--ling-sp-5)";
      root.appendChild(form);

      async function reload() {
        try {
          const result = await L.api("jobs.list");
          jobs = result.jobs || [];
        } catch (error) {
          L.toastError(error);
          return;
        }
        render();
      }

      function render() {
        tbody.innerHTML = "";
        empty.hidden = jobs.length > 0;
        table.hidden = jobs.length === 0;
        for (const job of jobs) {
          const row = el("tr");
          row.appendChild(el("td", null, job.name));
          const schedule = el("td");
          schedule.appendChild(el("span", "mono", job.cron || ""));
          const human = explain(job.cron || "");
          if (human) {
            schedule.appendChild(el("div", "small dim", human));
          }
          row.appendChild(schedule);
          row.appendChild(el("td", "mono", job.method || ""));
          row.appendChild(
            el("td", "small dim", job.last_run ? job.last_run : "还没触发过")
          );
          const actions = el("td");
          const toggle = el("button", null, job.enabled === false ? "启用" : "停用");
          toggle.disabled = busy;
          toggle.addEventListener("click", () => setEnabled(job, job.enabled === false));
          actions.appendChild(toggle);
          const remove = el("button", "ghost", "删除");
          remove.disabled = busy;
          remove.addEventListener("click", () => cancel(job));
          actions.appendChild(remove);
          row.appendChild(actions);
          tbody.appendChild(row);
        }
      }

      async function setEnabled(job, enabled) {
        busy = true;
        render();
        try {
          await L.api("jobs.setEnabled", { name: job.name, enabled });
          await reload();
        } catch (error) {
          L.toastError(error);
        } finally {
          busy = false;
          render();
        }
      }

      async function cancel(job) {
        const yes = await L.confirm({
          title: "删除任务",
          text: "删除定时任务 " + job.name + "？它以后不会再触发。",
          okText: "删除",
          danger: true
        });
        if (!yes) {
          return;
        }
        busy = true;
        try {
          await L.api("jobs.cancel", { name: job.name });
          await reload();
        } catch (error) {
          L.toastError(error);
        } finally {
          busy = false;
          render();
        }
      }

      function openForm() {
        form.hidden = false;
        form.innerHTML = "";
        const title = el("h2", null, "新增定时任务");
        title.style.fontSize = "var(--ling-fs-lg)";
        form.appendChild(title);

        const nameInput = document.createElement("input");
        nameInput.type = "text";
        const cronInput = document.createElement("input");
        cronInput.type = "text";
        cronInput.placeholder = "0 3 * * *";
        const methodInput = document.createElement("input");
        methodInput.type = "text";
        methodInput.placeholder = "插件名.方法名";
        const paramsArea = document.createElement("textarea");
        paramsArea.placeholder = "{}";
        paramsArea.value = "{}";
        const catchUp = document.createElement("input");
        catchUp.type = "checkbox";

        const nextRun = el("div", "small dim");
        cronInput.addEventListener("change", async () => {
          const cron = cronInput.value.trim();
          if (!cron) {
            nextRun.textContent = "";
            return;
          }
          try {
            const result = await L.api("jobs.validate", { cron });
            if (result && result.ok === false) {
              nextRun.textContent = result.error || "这个时间表不合法";
              nextRun.style.color = "var(--ling-danger)";
            } else {
              nextRun.textContent = result && result.next_run
                ? "下次触发：" + result.next_run
                : "";
              nextRun.style.color = "";
            }
          } catch (error) {
            nextRun.textContent = L.errorOf(error).message;
            nextRun.style.color = "var(--ling-danger)";
          }
        });

        function row(label, node) {
          const wrap = el("div", "row");
          wrap.appendChild(el("label", null, label));
          const control = el("div", "control");
          control.appendChild(node);
          wrap.appendChild(control);
          return wrap;
        }

        form.appendChild(row("名字", nameInput));
        form.appendChild(row("时间表", cronInput));
        form.appendChild(nextRun);
        form.appendChild(row("目标方法", methodInput));
        form.appendChild(row("参数（JSON）", paramsArea));
        form.appendChild(row("补跑", catchUp));

        const actions = el("div", "toolbar");
        const save = el("button", "primary", "保存");
        const cancelButton = el("button", null, "取消");
        actions.appendChild(save);
        actions.appendChild(cancelButton);
        form.appendChild(actions);

        cancelButton.addEventListener("click", () => {
          form.hidden = true;
        });
        save.addEventListener("click", async () => {
          let params = null;
          try {
            params = JSON.parse(paramsArea.value || "{}");
          } catch (error) {
            L.toast("参数不是合法的 JSON", "danger");
            return;
          }
          const name = nameInput.value.trim();
          const cron = cronInput.value.trim();
          const method = methodInput.value.trim();
          if (!name || !cron || !method) {
            L.toast("名字、时间表、目标方法都要填", "danger");
            return;
          }
          save.disabled = true;
          try {
            await L.api("jobs.schedule", {
              name,
              cron,
              method,
              params,
              catch_up: catchUp.checked
            });
            L.toast("已保存");
            form.hidden = true;
            await reload();
          } catch (error) {
            L.toastError(error);
          } finally {
            save.disabled = false;
          }
        });
      }

      addButton.addEventListener("click", openForm);
      reload();
      return { unmount() {} };
    }
  });
})();
