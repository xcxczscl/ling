/* 面板管理：外框、iframe、消息路由、宽度与恢复。
 *
 * 依据 docs/ling/builtin-plugins/core-ui/04-详细设计/03-面板挂载与消息协议.md。
 * 这个文件是外壳里唯一碰 event.source、postMessage 与 iframe 的地方，
 * 越权校验集中在 handleMessage 一处，审代码时不必在别处找。
 */
(function () {
  "use strict";

  const L = (window.Ling = window.Ling || {});

  const MAX_PANELS = 4;
  const MIN_WIDTH = 240;
  const MAX_INFLIGHT = 16;
  const CALL_TIMEOUT_MS = 35000;
  const LOAD_TIMEOUT_MS = 10000;
  const PRECHECK_TIMEOUT_MS = 5000;
  const SHUTDOWN_GRACE_MS = 300;
  const SAVE_DELAY_MS = 300;
  const MAX_DRAG_BYTES = 8 * 1024;

  const frames = new Map();   // id → 面板状态
  let order = [];             // 打开顺序，也就是左到右的顺序
  let widths = {};            // id → 占功能区的比例
  let onChange = function () {};
  let log = null;

  function say(message) {
    if (log) {
      log(message);
    }
  }

  function area() {
    return document.getElementById("panel-area");
  }

  function frameByWindow(source) {
    for (const entry of frames.values()) {
      if (entry.iframe && entry.iframe.contentWindow === source) {
        return entry;
      }
    }
    return null;
  }

  // ---------------- 布局 ----------------

  // 分隔条每次重排，避免「增删一个面板之后编号整体错位」这类问题。
  function relayout() {
    const host = area();
    if (!host) {
      return;
    }
    for (const splitter of host.querySelectorAll(".splitter")) {
      splitter.remove();
    }
    const share = 1 / Math.max(order.length, 1);
    order.forEach((id, index) => {
      const entry = frames.get(id);
      if (!entry) {
        return;
      }
      if (index > 0) {
        host.insertBefore(makeSplitter(), entry.section);
      }
      const value = typeof widths[id] === "number" ? widths[id] : share;
      entry.section.style.flex = value + " 1 0";
    });
  }

  function makeSplitter() {
    const splitter = document.createElement("div");
    splitter.className = "splitter";
    splitter.addEventListener("mousedown", (event) => startResize(event, splitter));
    splitter.addEventListener("dblclick", () => resetWidths());
    return splitter;
  }

  function startResize(event, splitter) {
    event.preventDefault();
    const all = [...area().querySelectorAll(".splitter")];
    const index = all.indexOf(splitter);
    const leftId = order[index];
    const rightId = order[index + 1];
    if (!leftId || !rightId) {
      return;
    }
    const left = frames.get(leftId).section;
    const right = frames.get(rightId).section;
    const total = area().clientWidth || 1;
    const startX = event.clientX;
    const leftWidth = left.getBoundingClientRect().width;
    const rightWidth = right.getBoundingClientRect().width;

    splitter.classList.add("active");
    const onMove = (moveEvent) => {
      const delta = moveEvent.clientX - startX;
      let nextLeft = leftWidth + delta;
      let nextRight = rightWidth - delta;
      // 拖到下限就停：两边的和不变，其余面板不受影响。
      if (nextLeft < MIN_WIDTH) {
        nextLeft = MIN_WIDTH;
        nextRight = leftWidth + rightWidth - MIN_WIDTH;
      }
      if (nextRight < MIN_WIDTH) {
        nextRight = MIN_WIDTH;
        nextLeft = leftWidth + rightWidth - MIN_WIDTH;
      }
      left.style.flex = nextLeft / total + " 1 0";
      right.style.flex = nextRight / total + " 1 0";
      widths[leftId] = nextLeft / total;
      widths[rightId] = nextRight / total;
    };
    const onUp = () => {
      splitter.classList.remove("active");
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      // 拖动过程中不写，松手 300 毫秒后才写。
      setTimeout(saveLayout, SAVE_DELAY_MS);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  function resetWidths() {
    const share = 1 / Math.max(order.length, 1);
    for (const id of order) {
      widths[id] = share;
    }
    relayout();
    saveLayout();
  }

  let saveTimer = 0;
  function saveLayout() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      const value = { order: [...order], widths: { ...widths } };
      L.api("config.set", { key: "core-ui.layout", value }).catch(() => {
        // 存不下只影响下次恢复，不影响本次使用，不打扰用户。
      });
    }, SAVE_DELAY_MS);
  }

  // ---------------- 外框内容 ----------------

  function showMessage(entry, text, path) {
    const body = entry.section.querySelector(".panel-body");
    if (!body) {
      return;
    }
    for (const old of body.querySelectorAll(".panel-message")) {
      old.remove();
    }
    const box = document.createElement("div");
    box.className = "panel-message";
    const line = document.createElement("div");
    line.textContent = text;
    box.appendChild(line);
    if (path) {
      const code = document.createElement("div");
      code.className = "path";
      code.textContent = path;
      box.appendChild(code);
    }
    const retry = document.createElement("button");
    retry.type = "button";
    retry.textContent = "重试";
    retry.addEventListener("click", () => {
      const id = entry.info.id;
      const info = entry.info;
      close(id, "closed", true);
      open(id, info);
    });
    box.appendChild(retry);
    body.appendChild(box);
  }

  function clearMessage(entry) {
    const body = entry.section.querySelector(".panel-body");
    if (body) {
      for (const old of body.querySelectorAll(".panel-message")) {
        old.remove();
      }
    }
  }

  // 插件停用或启动失败时的外框内容：说清楚状态，给一个「启用」。
  function showInactive(entry, info) {
    const body = entry.section.querySelector(".panel-body");
    if (!body) {
      return;
    }
    const box = document.createElement("div");
    box.className = "panel-message";
    const line = document.createElement("div");
    line.textContent = info.state === "failed" ? "插件启动失败" : "插件已停用";
    box.appendChild(line);
    if (info.state === "failed" && info.reason) {
      const reason = document.createElement("div");
      reason.className = "path";
      reason.textContent = info.reason;
      box.appendChild(reason);
    }
    const enable = document.createElement("button");
    enable.type = "button";
    enable.className = "primary";
    enable.textContent = "启用";
    enable.addEventListener("click", async () => {
      enable.disabled = true;
      try {
        await L.enablePlugin(info.id);
        const fresh = L.panelInfo(info.id) || info;
        close(info.id, "closed", true);
        open(info.id, fresh);
      } catch (error) {
        enable.disabled = false;
        L.toastError(error);
      }
    });
    box.appendChild(enable);
    body.appendChild(box);
  }

  async function precheck(url) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), PRECHECK_TIMEOUT_MS);
    try {
      const response = await fetch(url, { method: "HEAD", signal: controller.signal });
      return { status: response.status, ok: response.ok };
    } catch (error) {
      return { status: 0, ok: false, timeout: error && error.name === "AbortError" };
    } finally {
      clearTimeout(timer);
    }
  }

  // ---------------- 打开与关闭 ----------------

  async function open(id, info) {
    if (frames.has(id)) {
      // 已开：闪一下外框，不重复打开。
      const entry = frames.get(id);
      entry.section.classList.add("flash");
      setTimeout(() => entry.section.classList.remove("flash"), 300);
      return;
    }

    if (order.length >= MAX_PANELS) {
      const oldest = order[0];
      const oldestTitle = frames.get(oldest).info.title;
      close(oldest, "evicted", true);
      L.toast("已关闭较早打开的：" + oldestTitle, "info");
    }

    const url = info.entry || "/panel/" + id + "/";
    const check = await precheck(url);

    const section = document.createElement("section");
    section.className = "panel";
    section.dataset.plugin = id;

    const bar = document.createElement("header");
    bar.className = "panel-bar";
    const title = document.createElement("span");
    title.className = "panel-title";
    title.textContent = info.title;
    title.title = info.title;
    const closeButton = document.createElement("button");
    closeButton.type = "button";
    closeButton.className = "panel-close";
    closeButton.setAttribute("aria-label", "关闭");
    closeButton.textContent = "×";
    closeButton.addEventListener("click", () => close(id, "closed"));
    bar.appendChild(title);
    bar.appendChild(closeButton);

    const body = document.createElement("div");
    body.className = "panel-body";
    section.appendChild(bar);
    section.appendChild(body);

    const entry = {
      info,
      section,
      iframe: null,
      ready: false,
      pending: new Map(),
      loadTimer: 0
    };
    frames.set(id, entry);
    order.push(id);
    if (typeof widths[id] !== "number") {
      widths[id] = 1 / order.length;
    }
    area().appendChild(section);
    relayout();

    onChange();
    saveLayout();

    if (info.state && info.state !== "running") {
      // 停用或启动失败：不建 iframe，直接在外框里说清楚，并给一个「启用」。
      showInactive(entry, info);
      return;
    }

    if (!check.ok) {
      // 预检失败就不建 iframe：跨源 iframe 加载 404 时照样触发 load，
      // 外壳读不到里面，分不清「渲染了但空白」与「文件不存在」。
      if (check.timeout) {
        showMessage(entry, "面板文件读取超时", url);
      } else if (check.status === 404) {
        showMessage(entry, "面板文件不存在", url);
      } else {
        showMessage(entry, "面板打不开（HTTP " + check.status + "）", url);
      }
      return;
    }

    const iframe = document.createElement("iframe");
    // 不给 allow-same-origin：面板因此是不透明源，读不到外壳的存储，
    // 也发不出带 token 的请求（docs/ling/builtin-plugins/core-ui/06 §4）。
    iframe.setAttribute("sandbox", "allow-scripts allow-popups allow-downloads");
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.src = url;
    entry.iframe = iframe;
    body.appendChild(iframe);

    entry.loadTimer = setTimeout(() => {
      if (!entry.ready) {
        showMessage(entry, "面板加载超时", url);
      }
    }, LOAD_TIMEOUT_MS);

    iframe.addEventListener("load", () => {
      clearTimeout(entry.loadTimer);
      clearMessage(entry);
      entry.ready = true;
      post(entry, {
        type: "init",
        v: 1,
        pluginId: id,
        title: info.title,
        theme: L.theme.get(),
        tokens: L.theme.tokens()
      });
    });
  }

  function post(entry, message) {
    if (entry.iframe && entry.iframe.contentWindow) {
      // targetOrigin 只能是 "*"：面板自己是不透明源，指定不了别的。
      // 安全性由外壳这边校验 event.source 保证，不靠 targetOrigin。
      entry.iframe.contentWindow.postMessage(message, "*");
    }
  }

  function close(id, reason, immediate) {
    const entry = frames.get(id);
    if (!entry) {
      return;
    }
    if (entry.ready && entry.iframe) {
      // 先给面板一个收尾机会，再移除；到期无论如何都移除，不信任它配合。
      post(entry, { type: "shutdown", reason: reason || "closed" });
    }
    for (const pending of entry.pending.values()) {
      clearTimeout(pending.timer);
    }
    entry.pending.clear();
    clearTimeout(entry.loadTimer);

    const remove = () => {
      if (entry.section.parentNode) {
        entry.section.remove();
      }
      frames.delete(id);
      order = order.filter((item) => item !== id);
      relayout();
      onChange();
      saveLayout();
    };
    if (immediate || !entry.ready) {
      remove();
    } else {
      setTimeout(remove, SHUTDOWN_GRACE_MS);
    }
  }

  function closeAll(reason) {
    for (const id of [...order]) {
      close(id, reason, true);
    }
  }

  // ---------------- 恢复 ----------------

  async function restore(available) {
    let layout = null;
    try {
      const result = await L.api("config.get", { key: "core-ui.layout" });
      layout = result && result.value;
    } catch (error) {
      return;
    }
    if (!layout || !Array.isArray(layout.order)) {
      return;
    }
    widths = layout.widths && typeof layout.widths === "object" ? { ...layout.widths } : {};
    const gone = [];
    for (const id of layout.order.slice(0, MAX_PANELS)) {
      const info = available.find((item) => item.id === id);
      if (!info) {
        gone.push(id);  // 插件已卸载：不提示，用户卸的时候已经知道了
        continue;
      }
      if (info.state !== "running") {
        continue;  // 停用的不打开，入口变淡保留
      }
      await open(id, info);
    }
    if (gone.length > 0) {
      const kept = layout.order.filter((id) => !gone.includes(id));
      L.api("config.set", { key: "core-ui.layout", value: { order: kept, widths } }).catch(
        () => {}
      );
    }
  }

  // ---------------- 消息路由 ----------------

  // 只有该插件命名空间下的方法能过。这条不依赖 origin，也不看消息里写的 pluginId。
  function allowed(pluginId, method) {
    return typeof method === "string" && method.startsWith(pluginId + ".");
  }

  function reply(entry, id, payload) {
    post(entry, Object.assign({ type: "result", id }, payload));
  }

  function handleMessage(event) {
    const entry = frameByWindow(event.source);
    if (!entry) {
      return;  // 来源是别的窗口
    }
    // 面板是不透明源，发出来的消息 origin 恒为 "null"。
    // 同源页面发来的消息说明有人冒充面板，别放行（这条不能写成「相等才放行」）。
    if (event.origin !== "null") {
      say("来源不是不透明源，丢弃：origin=" + event.origin);
      return;
    }
    const message = event.data;
    if (!message || typeof message.type !== "string") {
      return;
    }
    if (message.type !== "call" && message.type !== "drag") {
      say("面板只允许发 call 与 drag，丢弃：" + message.type);
      return;
    }
    const id = message.id;
    if (!Number.isInteger(id) || id <= 0) {
      say("call/drag 缺 id 或 id 不是正整数，丢弃");
      return;
    }
    if (!entry.ready) {
      // 还没收到 init：回失败而不是静默丢弃，面板那边才知道自己早了一步。
      reply(entry, id, {
        ok: false,
        error: {
          code: "E_UI_NOT_READY",
          message: "面板还没收到 init 消息",
          advice: "等收到 init 之后再发 call"
        }
      });
      say("init 之前的调用，已回 E_UI_NOT_READY");
      return;
    }
    if (message.type === "drag") {
      startDrag(entry, message);
      return;
    }
    handleCall(entry, message);
  }

  function handleCall(entry, message) {
    const pluginId = entry.info.id;
    const id = message.id;
    if (typeof message.method !== "string") {
      reply(entry, id, {
        ok: false,
        error: { code: "E_PROTOCOL", message: "method 必须是字符串" }
      });
      return;
    }
    if (!allowed(pluginId, message.method)) {
      reply(entry, id, {
        ok: false,
        error: {
          code: "E_UI_FORBIDDEN",
          message: "面板不能调用 " + message.method,
          advice: "只能调用 " + pluginId + ".* 下的方法"
        }
      });
      say("plugin=" + pluginId + " method=" + message.method + " result=forbidden");
      return;
    }
    if (entry.pending.size >= MAX_INFLIGHT) {
      // 不排队：再多的请求多半是面板写错了循环。
      reply(entry, id, {
        ok: false,
        error: {
          code: "E_UI_BUSY",
          message: "同一时刻的请求太多",
          advice: "等前一批返回再发，或降低轮询频率"
        }
      });
      return;
    }
    // id 重复：旧条目立刻作废，此后到达的旧应答找不到条目，按 id 丢弃。
    const existing = entry.pending.get(id);
    if (existing) {
      clearTimeout(existing.timer);
      entry.pending.delete(id);
      reply(entry, id, {
        ok: false,
        error: { code: "E_TIMEOUT", message: "同 id 的新请求覆盖了这条" }
      });
    }
    const params = message.params && typeof message.params === "object" ? message.params : {};
    const timer = setTimeout(() => {
      if (entry.pending.has(id)) {
        entry.pending.delete(id);
        reply(entry, id, {
          ok: false,
          error: { code: "E_TIMEOUT", message: "ling 没有响应，正在忙或已卡住" }
        });
      }
    }, CALL_TIMEOUT_MS);
    entry.pending.set(id, { timer });

    L.api("panel/call", { plugin: pluginId, method: message.method, params })
      .then((result) => {
        const pending = entry.pending.get(id);
        if (!pending) {
          return;  // 已经超时或被覆盖
        }
        clearTimeout(pending.timer);
        entry.pending.delete(id);
        reply(entry, id, { ok: true, result });
      })
      .catch((error) => {
        const pending = entry.pending.get(id);
        if (!pending) {
          return;
        }
        clearTimeout(pending.timer);
        entry.pending.delete(id);
        reply(entry, id, { ok: false, error: L.errorOf(error) });
      });
  }

  // ---------------- 消息式拖拽 ----------------

  function startDrag(source, message) {
    const kind = message.kind;
    if (kind !== "files" && kind !== "text") {
      reply(source, message.id, {
        ok: false,
        error: { code: "E_PROTOCOL", message: "kind 只能是 files 或 text" }
      });
      return;
    }
    const payload = message.payload && typeof message.payload === "object" ? message.payload : {};
    if (JSON.stringify(payload).length > MAX_DRAG_BYTES) {
      reply(source, message.id, {
        ok: false,
        error: {
          code: "E_PAYLOAD_TOO_LARGE",
          message: "拖拽内容太大",
          advice: "拖拽内容过大，请传路径而不是内容"
        }
      });
      return;
    }

    const hint = document.getElementById("drag-hint");
    hint.textContent = kind === "files" ? "放到目标面板" : "放到目标面板（文本）";
    hint.hidden = false;

    // 落点按拖拽开始时的几何算：拖拽期间面板宽度一般不会变，
    // 每次都重新布局反而会让鼠标跟丢。
    const rects = order.map((id) => ({
      id,
      rect: frames.get(id).section.getBoundingClientRect()
    }));
    const move = (event) => {
      hint.style.left = event.clientX + 12 + "px";
      hint.style.top = event.clientY + 12 + "px";
    };
    const up = (event) => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      hint.hidden = true;
      const target = rects.find(
        (item) =>
          event.clientX >= item.rect.left &&
          event.clientX <= item.rect.right &&
          event.clientY >= item.rect.top &&
          event.clientY <= item.rect.bottom
      );
      if (!target) {
        reply(source, message.id, {
          ok: false,
          error: { code: "E_UI_FORBIDDEN", message: "没有落到任何面板上" }
        });
        return;
      }
      post(frames.get(target.id), {
        type: "event",
        name: "drop",
        data: { kind, payload, from: source.info.id }
      });
      // ok 只表示「已投递到某个面板」，不表示目标处理成功。
      reply(source, message.id, { ok: true, result: { delivered: true, to: target.id } });
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  }

  // ---------------- 对外 ----------------

  function broadcastTheme(theme, tokens) {
    for (const entry of frames.values()) {
      if (entry.ready) {
        post(entry, { type: "theme", theme, tokens });
      }
    }
  }

  function init(options) {
    onChange = (options && options.onChange) || function () {};
    log = (options && options.log) || null;
    window.addEventListener("message", handleMessage);
  }

  L.panels = {
    init,
    open,
    close,
    closeAll,
    restore,
    broadcastTheme,
    isOpen: (id) => frames.has(id),
    opened: () => [...order],
    setInfo: (id, info) => {
      const entry = frames.get(id);
      if (entry) {
        entry.info = info;
      }
    }
  };
})();
