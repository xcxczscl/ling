/* 面板侧样板（照 docs/ling/plugin-support/ui/02-消息与调用.md §8.3 的写法）。
 *
 * 三条要点都在这几行里：
 *   1. 守卫是 e.source === window.parent，不是 e.origin 白名单；
 *   2. 等 init 拿到 pluginId 再干活；
 *   3. 颜色只取 --ling-* 令牌。
 */
let pluginId = null;
let seq = 0;
const pending = new Map();
const logBox = document.getElementById("log");

function log(text) {
  const line = document.createElement("div");
  line.textContent = text;
  logBox.appendChild(line);
  logBox.scrollTop = logBox.scrollHeight;
}

window.addEventListener("message", (event) => {
  // 面板是不透明源，e.origin 恒为 "null"，拿它做白名单会把外壳自己的消息挡掉。
  if (event.source !== window.parent) {
    return;
  }
  const message = event.data;
  if (!message || typeof message.type !== "string") {
    return;
  }
  switch (message.type) {
    case "init":
      pluginId = message.pluginId;
      applyTheme(message.theme);
      log("收到 init，pluginId=" + pluginId + "，主题=" + message.theme);
      break;
    case "theme":
      applyTheme(message.theme);
      log("主题切到 " + message.theme);
      break;
    case "result": {
      const callback = pending.get(message.id);
      if (callback) {
        pending.delete(message.id);
        callback(message);
      }
      break;
    }
    case "event":
      if (message.name === "drop") {
        log("收到 drop：" + JSON.stringify(message.data));
        document.getElementById("drop").textContent =
          "收到了：" + JSON.stringify(message.data.payload);
      }
      break;
    case "shutdown":
      log("收到 shutdown：" + message.reason);
      break;
  }
});

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme === "dark" ? "dark" : "light");
}

function call(method, params) {
  if (!pluginId) {
    return Promise.reject({ code: "E_UI_NOT_READY", message: "还没收到 init" });
  }
  return new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, (message) => (message.ok ? resolve(message.result) : reject(message.error)));
    parent.postMessage({ type: "call", id, method, params: params || {} }, "*");
  });
}

function drag(kind, payload) {
  return new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, (message) => (message.ok ? resolve(message.result) : reject(message.error)));
    parent.postMessage({ type: "drag", id, kind, payload }, "*");
  });
}

document.getElementById("btn-echo").addEventListener("click", async () => {
  try {
    const result = await call("ui-demo.echo", { hello: "world", n: 42 });
    log("echo 成功：" + JSON.stringify(result));
  } catch (error) {
    log("echo 失败：" + JSON.stringify(error));
  }
});

document.getElementById("btn-boom").addEventListener("click", async () => {
  try {
    const result = await call("ui-demo.boom", {});
    log("boom 居然成功了：" + JSON.stringify(result));
  } catch (error) {
    log("boom 失败（预期）：" + JSON.stringify(error));
  }
});

document.getElementById("btn-sleep").addEventListener("click", async () => {
  const started = Date.now();
  try {
    await call("ui-demo.sleep", { seconds: 40 });
    log("sleep 返回了（不该这么快）");
  } catch (error) {
    log("sleep 失败（预期）用了 " + Math.round((Date.now() - started) / 1000) + " 秒：" +
        JSON.stringify(error));
  }
});

document.getElementById("btn-other").addEventListener("click", async () => {
  try {
    await call("core-settings.list", { prefix: "" });
    log("越权调用居然成功了（不该发生）");
  } catch (error) {
    log("越权调用被拦（预期）：" + JSON.stringify(error));
  }
});

document.getElementById("btn-drag").addEventListener("click", async () => {
  try {
    const result = await drag("files", { paths: ["D:/media/a.jpg"] });
    log("拖拽已投递：" + JSON.stringify(result));
  } catch (error) {
    log("拖拽失败：" + JSON.stringify(error));
  }
});

log("面板脚本已跑起来，等 init…");
