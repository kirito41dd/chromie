import { readFile } from "node:fs/promises";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

// 模拟浏览器时钟和网络，仅验证桥接边界，不执行真实浏览器或 GPU 绘制。
const source = (await readFile(new URL("../bootstrap.js", import.meta.url), "utf8"))
  .replace(/^import[^\n]+\n/, "");
const handoff = "Using exceptions for control flow, don't mind me. This isn't actually an error!";

function clock() {
  let now = 0;
  let id = 0;
  const timers = new Map();
  return {
    now: () => now,
    setTimeout(callback, delay) {
      const key = id++;
      timers.set(key, { callback, at: now + delay });
      return key;
    },
    clearTimeout(key) { timers.delete(key); },
    advance(duration) {
      const target = now + duration;
      while (true) {
        const next = [...timers].filter(([, timer]) => timer.at <= target)
          .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!next) break;
        timers.delete(next[0]);
        now = next[1].at;
        next[1].callback();
      }
      now = target;
    },
  };
}

async function boot({ failure = handoff, transport, pathname = "/spaces/work-notes/dashboard", search = "", remembered = "", saved, blockedStorage = false } = {}) {
  const loading = { hidden: false };
  const message = { textContent: "正在加载" };
  const viewport = { style: {} };
  const canvas = { tagName: "CANVAS", id: "canvas", parentElement: viewport, dataset: {} };
  const events = new Map();
  const documentCapture = new Map();
  // 同一目标允许多个同类型监听器，按注册顺序调用；stopPropagation 不取消同目标监听器。
  function addDocumentListener(name, handler, capture) {
    const previous = events.get(name);
    events.set(name, previous ? event => { previous(event); handler(event); } : handler);
    documentCapture.set(name, capture);
  }
  const responses = [];
  const accessResponses = [];
  const accessNotices = [];
  const messageResponses = [];
  const clipboard = [];
  const secretCopyNotices = [];
  let selectedText = "";
  const downloads = [];
  const blobs = new Map();
  const recallResponses = [];
  const modelResponses = [];
  const secretResponses = [];
  const secretVerifications = [];
  let secretHides = 0;
  const requests = [];
  const sizes = [];
  const initialized = [];
  const started = [];
  const initialPages = [];
  const pages = [];
  const navigation = [];
  const storage = new Map(remembered ? [["chromie.space", remembered]] : []);
  if (saved !== undefined) storage.set("chromie.saved-login", saved);
  const time = clock();
  let ended = 0;
  let reloads = 0;
  let accessResumes = 0;
  const address = { pathname, search, origin: "http://127.0.0.1:8732", reload: () => { reloads += 1; } };
  const pageDocument = { createElement: () => {
    const link = { href: "", download: "", click: () => downloads.push({ filename: link.download, blob: blobs.get(link.href) }) };
    return link;
  }, hidden: false, addEventListener: addDocumentListener, title: "", getElementById: id => ({ loading, "loading-message": message, viewport, canvas })[id] };
  const window = {
    innerWidth: 1180,
    innerHeight: 820,
    addEventListener(name, handler) { events.set(name, handler); },
    history: {
      pushState(state, _, path) { navigation.push({ replace: false, state, path }); address.pathname = path.split("?")[0]; address.search = path.includes("?") ? "?"+path.split("?")[1] : ""; },
      replaceState(state, _, path) { navigation.push({ replace: true, state, path }); address.pathname = path.split("?")[0]; address.search = path.includes("?") ? "?"+path.split("?")[1] : ""; },
    },
    localStorage: {
      getItem: key => { if (blockedStorage) throw new Error("虚构的存储拒绝"); return storage.get(key); },
      setItem: (key, value) => { if (blockedStorage) throw new Error("虚构的存储拒绝"); storage.set(key, value); },
      removeItem: key => { if (blockedStorage) throw new Error("虚构的存储拒绝"); storage.delete(key); },
    },
  };
  const browserNavigator = { clipboard: { writeText: async text => clipboard.push(text) } };
  const context = vm.createContext({
    window, Error, AbortController, TextEncoder, TextDecoder, Blob,
    URL: { createObjectURL: blob => { const url = "blob:test-" + blobs.size; blobs.set(url, blob); return url; }, revokeObjectURL: url => blobs.delete(url) },
    navigator: browserNavigator,
    setTimeout: time.setTimeout, clearTimeout: time.clearTimeout,
    location: address,
    document: pageDocument,
    performance: { now: time.now },
    init: async () => initialized.push({ ...viewport.style }),
    start: (space, recall) => { started.push(space); initialPages.push(recall); window.chromieReady(); if (failure) throw new Error(failure); },
    set_page: recall => pages.push(recall),
    set_access_page: access => pages.push(access ? "access" : false),
    set_messages_page: messages => pages.push(messages ? "messages" : false),
    set_settings_page: settings => pages.push(settings ? "settings" : false),
    receive_model: (...values) => modelResponses.push(values),
    receive_messages: (...values) => messageResponses.push(values),
    receive_access: (...values) => accessResponses.push(values),
    receive_access_notice: (...values) => accessNotices.push(values),
    resume_access: () => {accessResumes += 1;},
    hide_access_keys: () => window.chromieCancelAccess?.(),
    restore_session: () => {},
    resize_viewport: width => sizes.push(width),
    end_session: () => { ended += 1; },
    receive_snapshot: (...values) => responses.push(values),
    receive_recall: (...values) => recallResponses.push(values),
    receive_secret: (...values) => { secretResponses.push(values); return values[1]===200 && !values[3]; },
    selected_text: () => selectedText,
    receive_secret_copy: (...values) => secretCopyNotices.push(values),
    receive_secret_verification: value => secretVerifications.push(value),
    hide_secret: () => { secretHides += 1; window.chromieCancelSecret?.(); },
    fetch: async (url, options) => {
      requests.push({ url, options });
      return transport ? transport(url, options) : {
        ok: true, status: 200, text: async () => '{"fixture_only":true}',
      };
    },
  });
  await vm.runInContext(`(async () => { ${source}\n})()`, context);
  return { setSelection: text => { selectedText = text; }, navigator: browserNavigator, modelResponses, secretCopyNotices, accessNotices, messageResponses, accessResumes: () => accessResumes, downloads, accessResponses, clipboard, secretVerifications, secretResponses, secretHides: () => secretHides, window, viewport, canvas, documentCapture, initialized, loading, message, events, responses, recallResponses, requests, sizes, time, started, initialPages, pages, navigation, address, pageDocument, storage, ended: () => ended, reloads: () => reloads };
}

test("系统设置路由和模型配置只经同源 Cookie 接口，Key 不写入本地存储", async () => {
  const app = await boot({ pathname: "/spaces/work-notes/settings" });
  assert.equal(app.pageDocument.title, "Chromie · 系统设置");
  const payload = JSON.stringify({ api_key: "fictional-only-model-key" });
  await app.window.chromieModelRequest("work-notes", "save", payload, 3);
  assert.equal(app.requests[0].url, "/spaces/work-notes/system/model");
  assert.equal(app.requests[0].options.credentials, "same-origin");
  assert.equal(app.requests[0].options.redirect, "error");
  assert.equal(app.requests[0].options.cache, "no-store");
  assert.equal(app.requests[0].options.body, payload);
  assert.equal([...app.storage.values()].some(value => String(value).includes("fictional-only-model-key")), false);
  app.window.chromieNavigate("work-notes", false, false, false, false, true);
  assert.equal(app.address.pathname, "/spaces/work-notes/settings");
  app.events.get("popstate")();
  assert.equal(app.pages.at(-1), "settings");
});

test("离开模型页后忽略迟到响应，未知操作不发请求", async () => {
  let resolve;
  const app = await boot({ transport: () => new Promise(done => { resolve = done; }) });
  const pending = app.window.chromieModelRequest("work-notes", "read", "{}", 4);
  app.window.chromieCancelModel();
  resolve({ status: 200, text: async () => "{}" });
  await pending;
  assert.deepEqual(app.modelResponses, []);
  await app.window.chromieModelRequest("work-notes", "other", "{}", 5);
  assert.equal(app.requests.length, 1);
  assert.equal(app.modelResponses[0][1], 400);
});

test("独立测试连接经专用同源接口，不发送保存请求或持久保存 Key", async () => {
  const app = await boot({ pathname: "/spaces/work-notes/settings" });
  const payload = '{"api_key":"fictional-unsaved-model-key","model":"candidate-model"}';
  await app.window.chromieModelRequest("work-notes", "test", payload, 9);
  assert.equal(app.requests.length, 1);
  assert.equal(app.requests[0].url, "/spaces/work-notes/system/model/test");
  assert.equal(app.requests[0].options.method, "POST");
  assert.equal(app.requests[0].options.credentials, "same-origin");
  assert.equal(app.requests[0].options.redirect, "error");
  assert.equal(app.requests[0].options.body, payload);
  assert.equal([...app.storage.values()].some(value => String(value).includes("fictional-unsaved-model-key")), false);
});

test("保存模型替换未完成的进度读取，迟到旧响应不能覆盖保存结果", async () => {
  const replies = [];
  const app = await boot({ transport: () => new Promise(done => { replies.push(done); }) });
  const reading = app.window.chromieModelRequest("work-notes", "read", "{}", 4);
  const saving = app.window.chromieModelRequest("work-notes", "save", '{"model":"fictional-model"}', 5);
  assert.equal(app.requests[0].options.signal.aborted, true);
  replies[1]({ status: 200, text: async () => '{"revision":2}' });
  await saving;
  replies[0]({ status: 200, text: async () => '{"revision":1}' });
  await reading;
  assert.deepEqual(app.modelResponses, [[5, 200, '{"revision":2}']]);
});

test("HTTP 页面缺少 Clipboard API 时仍能复制，临时内容随即清空并恢复焦点", async () => {
  const app = await boot();
  delete app.navigator.clipboard;
  let selected, removed = false, restored = false;
  const field = { value: "", style: {}, select() { selected = this.value; }, remove() { removed = true; } };
  app.pageDocument.createElement = tag => { assert.equal(tag, "textarea"); return field; };
  app.pageDocument.body = { append: node => assert.equal(node, field) };
  app.pageDocument.activeElement = { focus: () => { restored = true; } };
  app.pageDocument.execCommand = operation => { assert.equal(operation, "copy"); return true; };
  await app.window.chromieCopyAccess("虚构接入配置", 7);
  assert.equal(selected, "虚构接入配置");
  assert.equal(field.value, "");
  assert.equal(removed && restored, true);
  assert.deepEqual(app.accessNotices, [[7, true]]);
});

test("复制被拒绝后不残留内容，不把失败报告为成功", async () => {
  const app = await boot();
  app.navigator.clipboard.writeText = async () => { throw new Error("虚构拒绝"); };
  let removed = false;
  const field = { value: "", style: {}, select() {}, remove() { removed = true; } };
  app.pageDocument.createElement = () => field;
  app.pageDocument.body = { append() {} };
  app.pageDocument.execCommand = () => false;
  await app.window.chromieCopyAccess("虚构配置", 8);
  assert.equal(field.value, "");
  assert.equal(removed, true);
  assert.deepEqual(app.accessNotices, [[8, false]]);
});

test("初始化 Wasm 前固定画布容器尺寸，只忽略已就绪的 Winit 控制流交接", async () => {
  const valid = await boot();
  assert.deepEqual(valid.initialized, [{ width: "1180px", height: "820px" }]);
  assert.equal(valid.loading.hidden, true);
  assert.deepEqual(valid.sizes, [1180]);
  const broken = await boot({ failure: "虚构的真实初始化错误" });
  assert.equal(broken.loading.hidden, false);
  assert.match(broken.message.textContent, /加载失败/);
});

test("连续拖动期间冻结实际容器与布局，最后一次事件后 200ms 只更新一次", async () => {
  const app = await boot();
  for (const [width, height] of [[900, 700], [680, 600], [390, 500]]) {
    app.window.innerWidth = width;
    app.window.innerHeight = height;
    app.events.get("resize")();
    app.time.advance(100);
    assert.deepEqual(app.viewport.style, { width: "1180px", height: "820px" });
    assert.deepEqual(app.sizes, [1180]);
  }
  app.time.advance(99);
  assert.deepEqual(app.sizes, [1180]);
  app.time.advance(1);
  assert.deepEqual(app.viewport.style, { width: "390px", height: "500px" });
  assert.deepEqual(app.sizes, [1180, 390]);
  app.time.advance(1000);
  assert.deepEqual(app.sizes, [1180, 390], "旧计时器不能补交过期尺寸");
});

test("只改高度也能生效，相同宽度不重复改布局，恢复大窗口仍能更新", async () => {
  const app = await boot();
  app.window.innerHeight = 400;
  app.events.get("resize")();
  app.time.advance(200);
  assert.deepEqual(app.viewport.style, { width: "1180px", height: "400px" });
  assert.deepEqual(app.sizes, [1180]);
  app.window.innerWidth = 1440;
  app.window.innerHeight = 900;
  app.events.get("resize")();
  app.time.advance(200);
  assert.deepEqual(app.viewport.style, { width: "1440px", height: "900px" });
  assert.deepEqual(app.sizes, [1180, 1440]);
});

test("离开页面取消未提交的尺寸，恢复页面时重新贴合窗口", async () => {
  const app = await boot();
  app.window.innerWidth = 390;
  app.window.innerHeight = 600;
  app.events.get("resize")();
  app.events.get("pagehide")();
  app.time.advance(1000);
  assert.deepEqual(app.viewport.style, { width: "1180px", height: "820px" });
  assert.deepEqual(app.sizes, [1180]);
  assert.equal(app.ended(), 1);
  app.events.get("pageshow")();
  assert.deepEqual(app.viewport.style, { width: "390px", height: "600px" });
  assert.deepEqual(app.sizes, [1180, 390]);
});

test("密码只发给登录接口，之后靠同源 Cookie 读取且只记住空间标识", async () => {
  const app = await boot();
  const password = "仅用于单元测试的虚构密码";
  await app.window.chromieRequest("fixture-space", "login", password, 7);
  const { url, options } = app.requests[0];
  const header = new Headers(options.headers).get("X-Chromie-Password");
  assert.equal(Buffer.from(header, "latin1").toString("utf8"), password);
  assert.equal(url, "/spaces/fixture-space/login");
  assert.equal(options.method, "POST");
  assert.equal(options.mode, "same-origin");
  assert.equal(options.cache, "no-store");
  assert.equal(options.redirect, "error");
  assert.equal(options.credentials, "same-origin");
  assert.equal(app.requests[1].url, "/spaces/fixture-space/dashboard-data");
  assert.equal(app.requests[1].options.headers, undefined);
  assert.equal(app.requests[1].options.credentials, "same-origin");
  assert.equal(app.responses[0][0], 7);
  assert.equal(app.responses[0][1], 200);
  assert.deepEqual([...app.storage], [["chromie.space", "fixture-space"]]);
});

test("根页面从最近空间恢复；已有空间地址优先于最近记录", async () => {
  const root = await boot({ pathname: "/", remembered: "demo" });
  assert.deepEqual(root.started, ["demo"]);
  const explicit = await boot({ remembered: "demo" });
  assert.deepEqual(explicit.started, ["work-notes"]);
});

test("登录失败不读取数据；主动退出与页面关闭分开", async () => {
  const denied = await boot({ transport: async () => ({ ok: false, status: 401 }) });
  await denied.window.chromieRequest("demo", "login", "虚构的错误密码", 1);
  assert.equal(denied.requests.length, 1);
  assert.equal(denied.responses[0][1], 401);
  assert.equal(denied.storage.size, 0);
  const app = await boot({ transport: async () => ({ ok: true, status: 204 }) });
  await app.window.chromieRequest("demo", "logout", "", 2);
  assert.equal(app.requests[0].url, "/spaces/demo/logout");
  assert.equal(app.requests[0].options.method, "POST");
  assert.equal(app.requests[0].options.headers, undefined);
  assert.equal(app.responses[0][1], 204);
  app.events.get("pagehide")();
  assert.equal(app.requests.length, 1, "关闭页面不能再次调用服务端退出");
});

test("新请求取消旧请求，页面离开时终止会话", async () => {
  let calls = 0;
  const app = await boot({ transport: async (_, { signal }) => {
    calls += 1;
    if (calls === 1) {
      return new Promise((_, reject) => {
        signal.addEventListener("abort", () => reject(new Error("虚构取消")), { once: true });
      });
    }
    return { ok: true, status: 200, text: async () => "{}" };
  } });
  const first = app.window.chromieRequest("fixture-space", "snapshot", "", 1);
  const second = app.window.chromieRequest("fixture-space", "snapshot", "", 2);
  await Promise.all([first, second]);
  assert.equal(app.requests[0].options.signal.aborted, true);
  assert.ok(!app.responses.some(([generation]) => generation === 1), "旧请求取消后不得回调或改写最近空间");
  assert.ok(app.responses.some(([generation, status]) => generation === 2 && status === 200));
  app.events.get("pagehide")();
  assert.equal(app.ended(), 1);
});

test("勾选且登录成功才保存密码，重开后按空间预填", async () => {
  const app = await boot();
  const password = "虚构的已验证密码-123";
  await app.window.chromieRequest("demo", "login", password, 1, true);
  const saved = app.storage.get("chromie.saved-login");
  assert.deepEqual(JSON.parse(saved), { space: "demo", password });
  assert.equal(app.window.chromieSavedPassword("demo"), password);
  assert.equal(app.window.chromieSavedPassword("work"), "");
  const reopened = await boot({ pathname: "/", saved });
  assert.deepEqual(reopened.started, ["demo"]);
  assert.equal(reopened.window.chromieSavedPassword("demo"), password);
  assert.equal(app.requests[1].options.headers, undefined);
  await app.window.chromieRequest("demo", "logout", "", 2);
  assert.equal(app.storage.get("chromie.saved-login"), saved, "退出登录保留主动记住的密码");
});

test("取消勾选立即清除，未勾选的登录不保存", async () => {
  const saved = JSON.stringify({ space: "demo", password: "虚构的旧密码-123" });
  const app = await boot({ saved });
  assert.equal(app.window.chromieForgetPassword(), true);
  assert.equal(app.window.chromieSavedPassword("demo"), "");
  assert.equal(app.storage.has("chromie.saved-login"), false);
  const unchecked = await boot({ saved });
  await unchecked.window.chromieRequest("demo", "login", "虚构的新密码-456", 1, false);
  assert.equal(unchecked.storage.has("chromie.saved-login"), false);
});

test("登录失败不保存，清除操作不会被延迟登录撤销", async () => {
  const denied = await boot({ transport: async () => ({ ok: false, status: 401 }) });
  await denied.window.chromieRequest("demo", "login", "虚构的错误密码-123", 1, true);
  assert.equal(denied.storage.has("chromie.saved-login"), false);
  let resolveLogin;
  const app = await boot({ transport: async url => {
    if (url.endsWith("/login")) return new Promise(resolve => { resolveLogin = resolve; });
    return { ok: true, status: 200, text: async () => "{}" };
  } });
  const pending = app.window.chromieRequest("demo", "login", "虚构的延迟密码-123", 1, true);
  app.window.chromieForgetPassword();
  resolveLogin({ ok: true, status: 204 });
  await pending;
  assert.equal(app.storage.has("chromie.saved-login"), false);
});

test("存储受限或记录损坏时仍可手动登录，并报告保存失败", async () => {
  const blocked = await boot({ blockedStorage: true });
  assert.equal(blocked.window.chromieSavedPassword("demo"), "");
  assert.equal(blocked.window.chromieForgetPassword(), false);
  await blocked.window.chromieRequest("demo", "login", "虚构的正常密码-123", 1, true);
  assert.equal(blocked.responses[0][1], 200);
  assert.equal(blocked.responses[0][3], true);
  for (const saved of ['{"损坏"', '{"space":"demo","password":5}', '{"space":"another-space","password":""}']) {
    const app = await boot({ saved });
    assert.equal(app.window.chromieSavedPassword("demo"), "");
  }
});

test("页面离开后的延迟登录不保存密码或恢复页面", async () => {
  let resolveLogin;
  const app = await boot({ transport: async () => new Promise(resolve => { resolveLogin = resolve; }) });
  const pending = app.window.chromieRequest("demo", "login", "虚构的延迟密码-123", 1, true);
  app.events.get("pagehide")();
  resolveLogin({ ok: true, status: 204 });
  await pending;
  assert.equal(app.storage.has("chromie.saved-login"), false);
  assert.equal(app.requests.length, 1);
  assert.equal(app.responses.length, 0);
});

test("召回使用 Cookie 和 POST 正文，不把查询写到网址或本地存储", async () => {
  const app = await boot();
  const query = "虚构查询 项目决定";
  await app.window.chromieRecall("demo", query, 3);
  const { url, options } = app.requests[0];
  assert.equal(url, "/spaces/demo/recall");
  assert.equal(options.method, "POST");
  assert.deepEqual(JSON.parse(options.body), { query, max_chars: 8000 });
  assert.equal(options.headers["Content-Type"], "application/json");
  assert.equal(options.headers["X-Chromie-Password"], undefined);
  assert.equal(options.credentials, "same-origin");
  assert.equal(options.redirect, "error");
  assert.equal(options.cache, "no-store");
  assert.equal(options.mode, "same-origin");
  assert.equal(app.responses.length, 0);
  assert.deepEqual(app.recallResponses[0].slice(0, 2), [3, 200]);
  assert.equal(app.storage.size, 0);
});

test("看板刷新与召回独立，清空搜索不取消看板请求", async () => {
  const pending = new Map();
  const app = await boot({ transport: async url => new Promise(resolve => { pending.set(url, resolve); }) });
  const search = app.window.chromieRecall("demo", "虚构关键词", 1);
  const snapshot = app.window.chromieRequest("demo", "snapshot", "", 2);
  assert.equal(app.requests[0].options.signal.aborted, false);
  app.window.chromieCancelRecall();
  assert.equal(app.requests[0].options.signal.aborted, true);
  assert.equal(app.requests[1].options.signal.aborted, false);
  for (const resolve of pending.values()) resolve({ ok: true, status: 200, text: async () => "{}" });
  await Promise.all([search, snapshot]);
  assert.equal(app.recallResponses.length, 0);
  assert.deepEqual(app.responses[0].slice(0, 2), [2, 200]);
});

test("新查询与页面离开丢弃旧召回响应", async () => {
  const pending = [];
  const app = await boot({ transport: async () => new Promise(resolve => { pending.push(resolve); }) });
  const first = app.window.chromieRecall("demo", "第一个虚构查询", 1);
  const second = app.window.chromieRecall("demo", "第二个虚构查询", 2);
  assert.equal(app.requests[0].options.signal.aborted, true);
  pending[1]({ ok: true, status: 200, text: async () => "{}" });
  pending[0]({ ok: true, status: 200, text: async () => "{}" });
  await Promise.all([first, second]);
  assert.equal(app.recallResponses.length, 1);
  assert.equal(app.recallResponses[0][0], 2);
  const leaving = app.window.chromieRecall("demo", "离开前的虚构查询", 3);
  app.events.get("pagehide")();
  pending[2]({ ok: true, status: 200, text: async () => "{}" });
  await leaving;
  assert.equal(app.recallResponses.length, 1);
});

test("召回超时和鉴权失败都有独立状态返回", async () => {
  const timed = await boot({ transport: async (_, { signal }) => new Promise((_, reject) => {
    signal.addEventListener("abort", () => reject(new Error("虚构超时")), { once: true });
  }) });
  const pending = timed.window.chromieRecall("demo", "虚构查询", 4);
  timed.time.advance(10000);
  await pending;
  assert.deepEqual(timed.recallResponses[0].slice(0, 2), [4, 0]);
  const denied = await boot({ transport: async () => ({ ok: false, status: 401, text: async () => "" }) });
  await denied.window.chromieRecall("demo", "虚构查询", 5);
  assert.deepEqual(denied.recallResponses[0].slice(0, 3), [5, 401, ""]);
});

test("看板和召回页有独立地址，导航不携带查询或鉴权信息", async () => {
  const dashboard = await boot();
  assert.deepEqual(dashboard.initialPages, [false]);
  const recall = await boot({ pathname: "/spaces/demo/recall" });
  assert.deepEqual(recall.started, ["demo"]);
  assert.deepEqual(recall.initialPages, [true]);
  assert.equal(recall.pageDocument.title, "Chromie · 召回调试");
  dashboard.window.chromieNavigate("work-notes", true, false);
  assert.equal(dashboard.address.pathname, "/spaces/work-notes/recall");
  assert.deepEqual(dashboard.navigation[0], { replace: false, state: null, path: "/spaces/work-notes/recall" });
  dashboard.window.chromieNavigate("work-notes", true, false);
  assert.equal(dashboard.navigation.length, 1);
  dashboard.window.chromieNavigate("demo", false, true);
  assert.deepEqual(dashboard.navigation[1], { replace: true, state: null, path: "/spaces/demo/dashboard" });
  dashboard.window.chromieNavigate("../../不合法空间", true, false);
  assert.equal(dashboard.navigation.length, 2);
});

test("浏览器前进后退恢复页面，跨空间历史记录重新加载", async () => {
  const app = await boot();
  app.address.pathname = "/spaces/work-notes/recall";
  app.events.get("popstate")();
  assert.deepEqual(app.pages, [true]);
  app.address.pathname = "/spaces/work-notes/dashboard";
  app.events.get("popstate")();
  assert.deepEqual(app.pages, [true, false]);
  app.address.pathname = "/spaces/demo/recall";
  app.events.get("popstate")();
  assert.equal(app.reloads(), 1);
  assert.equal(app.ended(), 1);
  assert.deepEqual(app.pages, [true, false]);
});

test("跨空间历史跳转立即取消旧请求，延迟响应不能恢复旧内容", async () => {
  let resolveResponse;
  const app = await boot({ pathname: "/spaces/work-notes/recall", transport: async () => new Promise(resolve => { resolveResponse = resolve; }) });
  const pending = app.window.chromieRecall("work-notes", "虚构旧空间查询", 8);
  app.address.pathname = "/spaces/demo/recall";
  app.events.get("popstate")();
  assert.equal(app.requests[0].options.signal.aborted, true);
  resolveResponse({ ok: true, status: 200, text: async () => "{}" });
  await pending;
  assert.equal(app.recallResponses.length, 0);
});

test("调试耗时取自实际请求往返，保留错误响应正文", async () => {
  let resolveResponse;
  const app = await boot({ transport: async () => new Promise(resolve => { resolveResponse = resolve; }) });
  const pending = app.window.chromieRecall("demo", "虚构查询", 6);
  app.time.advance(37);
  const body = '{"code":"unavailable","message":"虚构测试错误"}';
  resolveResponse({ ok: false, status: 503, text: async () => body });
  await pending;
  assert.deepEqual(app.recallResponses[0].slice(0, 4), [6, 503, body, 37]);
  assert.match(app.recallResponses[0][4], /^\d{4}-\d{2}-\d{2}T.*Z$/);
  assert.equal(app.storage.size, 0);
});

const secretId = "01900000-0000-7000-8000-000000000000";
function secretResponse(value = "虚构凭据", { status = 200, type = "application/octet-stream" } = {}) {
  const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value;
  return { ok: status === 200, status, headers: { get: () => type }, arrayBuffer: async () => bytes.slice().buffer };
}

test("Secret 独立验密且 Cookie 不参与，原文不进入召回调试或浏览器存储，60 秒后隐藏", async () => {
  const password = "仅用于测试的空间密码";
  const app = await boot({ transport: async () => secretResponse("虚构原文\n第二行") });
  await app.window.chromieRevealSecret("demo", secretId, password, 1);
  const { url, options } = app.requests[0];
  assert.equal(url, `/spaces/demo/secrets/${secretId}`);
  assert.equal(options.method, "GET");
  assert.equal(options.headers["X-Chromie-Password"], String.fromCharCode(...new TextEncoder().encode(password)));
  assert.equal(options.credentials, "omit");
  assert.equal(options.redirect, "error");
  assert.equal(options.mode, "same-origin");
  assert.equal(options.cache, "no-store");
  assert.equal(options.body, undefined);
  assert.deepEqual(app.secretResponses[0], [1, 200, "虚构原文\n第二行", false]);
  assert.equal(app.responses.length,0);
  assert.equal(app.recallResponses.length,0);
  assert.equal(app.storage.size,0);
  app.time.advance(59999);
  assert.equal(app.secretHides(),0);
  app.time.advance(1);
  assert.equal(app.secretHides(),1);
});

test("关闭、切后台和切页取消取密，迟到原文不能重新显示", async () => {
  for (const operation of ["close","background","pagehide","navigate","logout"]) {
    let resolveResponse;
    const app = await boot({ transport: async url => url.endsWith(secretId) ? new Promise(resolve => {resolveResponse=resolve;}) : ({ok:true,status:204}) });
    const pending = app.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",2);
    if (operation === "close") app.window.chromieCancelSecret();
    if (operation === "background") {app.pageDocument.hidden=true;app.events.get("visibilitychange")();}
    if (operation === "pagehide") app.events.get("pagehide")();
    if (operation === "navigate") app.window.chromieNavigate("work-notes",true,false);
    if (operation === "logout") await app.window.chromieRequest("work-notes","logout","",9);
    assert.equal(app.requests[0].options.signal.aborted,true,operation);
    resolveResponse(secretResponse("迟到的虚构原文"));
    await pending;
    assert.equal(app.secretResponses.length,0,operation);
  }
});

test("响应正文读取期间切换 Secret 也不能回填旧值，看板刷新不打断取密", async () => {
  let releaseBody;
  let enteredBody;
  const bodyStarted = new Promise(resolve => {enteredBody=resolve;});
  const app=await boot({transport:async url => url.endsWith(secretId) ? {
    ...secretResponse(),arrayBuffer:async()=>{enteredBody();return new Promise(resolve=>{releaseBody=resolve;});},
  } : {ok:true,status:200,text:async()=>"{}"}});
  const pending=app.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",3);
  await bodyStarted;
  await app.window.chromieRequest("demo","snapshot","",4);
  assert.equal(app.requests[0].options.signal.aborted,false);
  app.window.chromieCancelSecret();
  releaseBody(new TextEncoder().encode("虚构旧原文").buffer);
  await pending;
  assert.equal(app.secretResponses.length,0);
  assert.equal(app.responses.length,1);
});

test("错误密码和超时只返回状态，不读取错误正文或退出原登录", async () => {
  const denied=await boot({transport:async()=>({ok:false,status:401,arrayBuffer:async()=>{throw new Error("不应读取错误正文");}})});
  await denied.window.chromieRevealSecret("demo",secretId,"错误的虚构空间密码",5);
  assert.deepEqual(denied.secretResponses[0],[5,401,"",false]);
  assert.equal(denied.ended(),0);
  const timed=await boot({transport:async(_, {signal})=>new Promise((_,reject)=>signal.addEventListener("abort",()=>reject(new Error("虚构超时")),{once:true}))});
  const request=timed.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",6);
  timed.time.advance(10000);
  await request;
  assert.deepEqual(timed.secretResponses[0],[6,0,"",false]);
});

test("二进制、控制字符、超长和错误类型不进入显示，UTF-8 原文保持字符", async () => {
  for (const response of [secretResponse(new Uint8Array([255])),secretResponse("原文\0控制字符"),secretResponse("原文\u0085控制字符"),secretResponse("x".repeat(60001)),secretResponse("不是凭据",{type:"text/html"})]) {
    const app=await boot({transport:async()=>response});
    await app.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",7);
    assert.deepEqual(app.secretResponses[0],[7,200,"",true]);
    app.time.advance(60000);
    assert.equal(app.secretHides(),0);
  }
  const unicode=await boot({transport:async()=>secretResponse("\ufeff虚构原文\r\n保留换行",{type:"Application/Octet-Stream; charset=utf-8"})});
  await unicode.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",8);
  assert.equal(unicode.secretResponses[0][2],"\ufeff虚构原文\r\n保留换行");
});

test("非法对象地址或空密码不发请求，手动取消清除自动隐藏计时", async () => {
  const app=await boot({transport:async()=>secretResponse()});
  for (const args of [["../demo",secretId,"虚构测试空间密码"],["demo","../secrets","虚构测试空间密码"],["demo",secretId,""]]) {
    await app.window.chromieRevealSecret(...args,10);
  }
  assert.equal(app.requests.length,0);
  await app.window.chromieRevealSecret("demo",secretId,"虚构测试空间密码",11);
  app.window.chromieCancelSecret();
  app.time.advance(60000);
  assert.equal(app.secretHides(),0);
});


test("记住的登录密码不替代首次 Secret 手动验证，ASCII 原文也能独立显示", async () => {
  const saved=JSON.stringify({space:"demo",password:"outdated-browser-password"});
  const app=await boot({saved,transport:async()=>secretResponse("demo-only-credential-123")});
  await app.window.chromieRevealSecret("demo",secretId,"chromie-demo",12);
  assert.equal(app.requests[0].options.headers["X-Chromie-Password"],"chromie-demo");
  assert.equal(app.storage.get("chromie.saved-login"),saved);
  assert.equal(app.storage.size,1);
  assert.equal(app.secretResponses[0][2],"demo-only-credential-123");
  app.window.chromieCancelSecret();
});

test("同空间多个 Secret、隐藏关闭及同空间切页复用验密，固定五分钟不续期", async () => {
  const app = await boot({ pathname: "/spaces/demo/dashboard", transport: async () => secretResponse() });
  assert.equal(app.window.chromieSecretVerified("demo"), false);
  await app.window.chromieRevealSecret("demo", secretId, "manual-demo-password", 20);
  assert.equal(app.window.chromieSecretVerified("demo"), true);
  assert.equal(app.window.chromieSecretVerified("work"), false);
  app.window.chromieCancelSecret();
  app.time.advance(240000);
  app.window.chromieNavigate("demo", true, false);
  await app.window.chromieRevealSecret("demo", "01900000-0000-7000-8000-000000000001", "", 21);
  assert.equal(app.requests[1].options.headers["X-Chromie-Password"], "manual-demo-password");
  assert.equal(app.secretResponses.at(-1)[1], 200);
  app.time.advance(59999);
  assert.equal(app.window.chromieSecretVerified("demo"), true);
  app.time.advance(1);
  assert.equal(app.window.chromieSecretVerified("demo"), false);
  const count = app.requests.length;
  await app.window.chromieRevealSecret("demo", secretId, "", 22);
  assert.equal(app.requests.length, count);
  assert.equal(app.storage.size, 0);
  await app.window.chromieRevealSecret("demo", secretId, "reentered-demo-password", 23);
  assert.equal(app.window.chromieSecretVerified("demo"), true);
  app.time.advance(300000);
});

test("切后台隐藏原文但保留验证，刷新、退出、换空间及新登录清除验证", async () => {
  for (const operation of ["background", "pagehide", "logout", "space", "login"]) {
    const app = await boot({ pathname: "/spaces/demo/dashboard", transport: async url => url.includes("/secrets/") ? secretResponse() : ({ok:true,status:200,text:async()=>"{}"}) });
    await app.window.chromieRevealSecret("demo", secretId, "manual-demo-password", 24);
    if (operation === "background") { app.pageDocument.hidden=true; app.events.get("visibilitychange")(); }
    if (operation === "pagehide") app.events.get("pagehide")();
    if (operation === "logout") await app.window.chromieRequest("demo", "logout", "", 1);
    if (operation === "space") app.window.chromieNavigate("work", false, false);
    if (operation === "login") await app.window.chromieRequest("demo", "login", "manual-demo-password", 1);
    assert.equal(app.window.chromieSecretVerified("demo"), operation === "background", operation);
    if (operation === "background") assert.equal(app.secretHides(), 1);
    app.time.advance(300000);
  }
  const fresh = await boot({saved: JSON.stringify({space:"demo",password:"manual-demo-password"})});
  assert.equal(fresh.window.chromieSecretVerified("demo"), false, "刷新或新标签不能从浏览器存储恢复本次验密");
});

test("未验密不能跨空间复用，改密后的 401 清除缓存且保留原登录界面", async () => {
  let revoked = false;
  const app = await boot({transport: async () => secretResponse("虚构凭据", {status: revoked ? 401 : 200})});
  await app.window.chromieRevealSecret("demo", secretId, "manual-demo-password", 25);
  await app.window.chromieRevealSecret("work", secretId, "", 26);
  assert.equal(app.requests.length, 1);
  revoked = true;
  await app.window.chromieRevealSecret("demo", secretId, "", 27);
  assert.deepEqual(app.secretResponses.at(-1), [27,401,"",false]);
  assert.equal(app.window.chromieSecretVerified("demo"), false);
  assert.equal(app.ended(), 0);
  assert.equal(app.storage.size, 0);
});

test("验证到期取消在途缓存请求，迟到原文不能显示或延长验证", async () => {
  let releaseBody;
  let bodyStarted;
  const enteredBody = new Promise(resolve => {bodyStarted=resolve;});
  let count = 0;
  const app = await boot({transport: async () => {
    if (count++ === 0) return secretResponse();
    return {...secretResponse(), arrayBuffer: async () => {bodyStarted(); return new Promise(resolve => {releaseBody=resolve;});}};
  }});
  await app.window.chromieRevealSecret("demo", secretId, "manual-demo-password", 28);
  app.window.chromieCancelSecret();
  app.time.advance(299999);
  const pending = app.window.chromieRevealSecret("demo", secretId, "", 29);
  await enteredBody;
  app.time.advance(1);
  assert.equal(app.requests[1].options.signal.aborted, true);
  releaseBody(new TextEncoder().encode("迟到的虚构原文").buffer);
  await pending;
  assert.equal(app.secretResponses.length, 1);
  assert.equal(app.window.chromieSecretVerified("demo"), false);
});

test("失败、二进制及关闭后的迟到首次响应不建立验证缓存", async () => {
  for (const response of [secretResponse("",{status:401}),secretResponse(new Uint8Array([255]))]) {
    const app = await boot({transport:async()=>response});
    await app.window.chromieRevealSecret("demo",secretId,"manual-demo-password",30);
    assert.equal(app.window.chromieSecretVerified("demo"),false);
  }
  let release;
  const app = await boot({transport:async()=>new Promise(resolve=>{release=resolve;})});
  const pending = app.window.chromieRevealSecret("demo",secretId,"manual-demo-password",31);
  app.window.chromieCancelSecret();
  release(secretResponse());
  await pending;
  assert.equal(app.window.chromieSecretVerified("demo"),false);
});

test("Wasm 尚未就绪时离开页面只清理内存，不调用验证导出", async () => {
  const app = await boot({failure:"虚构的初始化失败"});
  app.events.get("pagehide")();
  assert.equal(app.secretVerifications.length, 0);
});

function imeKey(app, type, target, key, keyCode, isComposing = false, modifiers = {}) {
  let stopped = false;
  app.events.get(type)?.({
    target, key, keyCode, isComposing, ...modifiers,
    stopPropagation: () => { stopped = true; },
    preventDefault: () => { throw new Error("不能阻止浏览器输入法的默认编辑"); },
  });
  return stopped;
}

test("IME 首尾键在 isComposing 为 false 时也不能作为字母或提交键传入 Slint", async () => {
  const app = await boot();
  const helper = {tagName:"INPUT",parentElement:app.viewport,nextElementSibling:app.canvas};
  for (const target of [helper, app.canvas]) {
    for (const type of ["keydown", "keyup"]) {
      assert.equal(app.documentCapture.get(type), true, "在 SDK 目标监听器之前拦截");
      assert.equal(imeKey(app,type,target,"n",229,false), true, "首键尚未开始组合也不能提前写入 n");
      assert.equal(imeKey(app,type,target,"Enter",229,false), true, "选词确认不能误触发搜索");
      assert.equal(imeKey(app,type,target,"n",78,true), true, "组合期间按键不重复转为文本");
      assert.equal(imeKey(app,type,target,"Enter",13,true), true);
    }
  }
});

test("普通英文、编辑键和快捷键继续传入，选词后的真正 Enter 仍可搜索", async () => {
  const app = await boot();
  const helper = {tagName:"INPUT",parentElement:app.viewport,nextElementSibling:app.canvas};
  for (const type of ["keydown", "keyup"]) {
    for (const [key, keyCode, modifiers] of [["n",78,{}],["Enter",13,{}],["Backspace",8,{}],["Escape",27,{}],["Tab",9,{}],["a",65,{metaKey:true}],["v",86,{ctrlKey:true}]]) {
      assert.equal(imeKey(app,type,helper,key,keyCode,false,modifiers), false, key);
    }
  }
});

test("只处理本画布和相邻辅助输入，不拦截其他表单或读取输入值", async () => {
  const app = await boot();
  const targets = [
    {tagName:"INPUT",parentElement:{},nextElementSibling:app.canvas},
    {tagName:"INPUT",parentElement:app.viewport,nextElementSibling:{}},
    {tagName:"TEXTAREA",parentElement:app.viewport,nextElementSibling:app.canvas},
    {}, null,
  ];
  for (const target of targets) assert.equal(imeKey(app,"keydown",target,"n",229,true),false);
  const helper = {tagName:"INPUT",parentElement:app.viewport,nextElementSibling:app.canvas,
    get value() {throw new Error("不能读取或记录输入内容");}};
  assert.equal(imeKey(app,"keydown",helper,"n",229,false),true);
  assert.equal(app.storage.size,0);
});

test("原生 composition 与 input 事件没有被替换或阻止", async () => {
  const app = await boot();
  for (const type of ["compositionstart","compositionupdate","compositionend","beforeinput","input","paste"]) {
    assert.equal(app.events.has(type),false, `${type} 应继续由 SDK 处理`);
  }
});


test("接入页面支持独立地址、顶栏和历史导航", async () => {
  const app = await boot({ pathname: "/spaces/work-notes/access" });
  assert.equal(app.pageDocument.title, "Chromie · 接入");
  app.window.chromieNavigate("work-notes", false, false, true);
  assert.equal(app.address.pathname, "/spaces/work-notes/access");
  app.events.get("popstate")();
  assert.deepEqual(app.pages, ["access"]);
});

test("接入管理只携带同源 Cookie，查看密钥使用显式 POST 且不记录本地存储", async () => {
  const fixtureKey = "cm_mcp_" + "a".repeat(64);
  const app = await boot({ transport: async () => ({ ok: true, status: 200, text: async () => JSON.stringify({ key: fixtureKey }) }) });
  const id = "01900000-0000-7000-8000-000000000000";
  await app.window.chromieAccess("work-notes", "reveal", id, "mcp", "{}", 8);
  assert.equal(app.requests.length, 1);
  assert.equal(app.requests[0].url, `/spaces/work-notes/agents/${id}/keys/mcp/reveal`);
  assert.equal(app.requests[0].options.method, "POST");
  assert.equal(app.requests[0].options.credentials, "same-origin");
  assert.equal(app.requests[0].options.redirect, "error");
  assert.equal(app.requests[0].options.cache, "no-store");
  assert.equal(app.requests[0].options.headers, undefined);
  assert.equal(app.requests[0].options.body, undefined);
  assert.ok(!JSON.stringify([...app.storage]).includes(fixtureKey));
});

test("切页取消接入请求，迟到的密钥响应不再交给界面", async () => {
  let finish;
  const app = await boot({ transport: () => new Promise(resolve => { finish = resolve; }) });
  const request = app.window.chromieAccess("work-notes", "reveal", "01900000-0000-7000-8000-000000000000", "hook", "{}", 9);
  app.window.chromieCancelAccess();
  finish({ ok: true, status: 200, text: async () => JSON.stringify({ key: "cm_hook_" + "b".repeat(64) }) });
  await request;
  assert.equal(app.accessResponses.length, 0);
  assert.equal(app.requests[0].options.signal.aborted, true);
});

test("接入管理拒绝无效用途、外部路径与额外操作", async () => {
  const app = await boot();
  for (const [id, kind, operation] of [["../other", "mcp", "reveal"], ["01900000-0000-7000-8000-000000000000", "../hook", "reveal"], ["", "", "delete"]]) {
    await app.window.chromieAccess("work-notes", operation, id, kind, "{}", 1);
  }
  assert.equal(app.requests.length, 0);
  assert.equal(app.accessResponses.length, 3);
  assert.ok(app.accessResponses.every(row => row[1] === 400));
});

test("下载期间取消后不继续读取第二枚密钥", async () => {
  let finish;
  const app = await boot({ transport: () => new Promise(resolve => { finish = resolve; }) });
  const request = app.window.chromieAccess("work-notes", "download", "01900000-0000-7000-8000-000000000000", "", "{}", 10);
  app.window.chromieCancelAccess();
  finish({ ok: true, status: 200, json: async () => ({ key: "cm_mcp_" + "a".repeat(64) }) });
  await request;
  assert.equal(app.requests.length, 1);
  assert.equal(app.accessResponses.length, 0);
});


test("主动下载生成当前接入的完整私有 JSON，密钥不写入浏览器存储", async () => {
  const app = await boot({ transport: async url => ({ ok: true, status: 200, json: async () => ({ key: url.includes("/mcp/") ? "cm_mcp_" + "a".repeat(64) : "cm_hook_" + "b".repeat(64) }) }) });
  const id = "01900000-0000-7000-8000-000000000000";
  await app.window.chromieAccess("work-notes", "download", id, "", "{}", 11);
  assert.equal(app.downloads.length, 1);
  assert.equal(app.downloads[0].filename, `chromie-${id}.json`);
  const value = JSON.parse(await app.downloads[0].blob.text());
  assert.equal(value.url, "http://127.0.0.1:8732");
  assert.equal(value.space, "work-notes");
  assert.equal(value.agent_id, id);
  assert.match(value.mcp_key, /^cm_mcp_[a-f0-9]{64}$/);
  assert.match(value.hook_key, /^cm_hook_[a-f0-9]{64}$/);
  assert.ok(!JSON.stringify([...app.storage]).includes(value.mcp_key));
  assert.equal(app.accessResponses[0][1], 200);
});

test("CLI 配对仅从公开请求 ID 恢复，同源 Cookie 预览与显式批准，不读取 Agent 密钥", async () => {
  const id = "01a182bf-2655-7799-8219-56cf70cbda65";
  const app = await boot({ pathname: "/spaces/work-notes/access", search: `?pair=${id}` });
  assert.equal(app.window.chromiePairingId(), id);
  await app.window.chromieAccess("work-notes", "pair-info", id, "", "{}", 11);
  assert.equal(app.requests.at(-1).url, `/spaces/work-notes/agents/pairings/${id}`);
  assert.equal(app.requests.at(-1).options.method, "GET");
  await app.window.chromieAccess("work-notes", "pair-approve", id, "", JSON.stringify({ agent_id: id, code: "fictional-code" }), 12);
  const request = app.requests.at(-1);
  assert.equal(request.options.credentials, "same-origin");
  assert.equal(request.options.method, "POST");
  assert.equal(request.options.headers.Authorization, undefined);
  assert.deepEqual(JSON.parse(request.options.body), { agent_id: id, code: "fictional-code" });
  assert.equal(app.requests.some(request => request.url.includes("/keys/")), false);
  app.address.pathname = "/spaces/other/access";
  assert.equal(app.window.chromiePairingId(), "", "不能跨空间携带待确认配对");
});

test("返回管理会清除公开配对参数，历史切换按当前 URL 恢复请求", async () => {
  const id = "01a182bf-2655-7799-8219-56cf70cbda65";
  const app = await boot({ pathname: "/spaces/work-notes/access", search: `?pair=${id}` });
  app.window.chromieDismissPairing();
  assert.equal(app.address.search, "");
  assert.equal(app.window.chromiePairingId(), "");
  app.window.history.pushState(null, "", `/spaces/work-notes/access?pair=${id}`);
  app.events.get("popstate")();
  assert.equal(app.window.chromiePairingId(), id);
  assert.equal(app.pages.at(-1), "access");
});

test("切回接入页恢复被中断的状态读取，其他页面不触发接入刷新", async () => {
  const access = await boot({ pathname: "/spaces/work-notes/access" });
  access.pageDocument.hidden = true;
  access.events.get("visibilitychange")();
  assert.equal(access.accessResumes(), 0);
  access.pageDocument.hidden = false;
  access.events.get("visibilitychange")();
  assert.equal(access.accessResumes(), 1);
  const dashboard = await boot();
  dashboard.events.get("visibilitychange")();
  assert.equal(dashboard.accessResumes(), 0);
});


test("终端偏好只保存 Bash 或 Nushell，存储不可用不影响当前页面", async () => {
  const app = await boot();
  assert.equal(app.window.chromieUsesNushell(), false);
  app.window.chromieSetShell(true);
  assert.equal(app.storage.get("chromie.shell"), "nushell");
  assert.equal(app.window.chromieUsesNushell(), true);
  app.window.chromieSetShell(false);
  assert.equal(app.storage.get("chromie.shell"), "bash");
  const blocked=await boot({blockedStorage:true});
  assert.doesNotThrow(()=>blocked.window.chromieSetShell(true));
  assert.equal(blocked.window.chromieUsesNushell(),false);
});


test("采集页独立地址和历史导航，查询只进正文并使用同源 Cookie", async () => {
  const app = await boot({ pathname: "/spaces/work-notes/messages" });
  assert.equal(app.pageDocument.title,"Chromie · 采集消息");
  app.events.get("popstate")();
  assert.equal(app.pages.at(-1),"messages");
  const payload={agent_id:null,session_id:"fictional-private-session",page:0};
  await app.window.chromieMessages("work-notes","conversation",JSON.stringify(payload),1);
  const request=app.requests.at(-1);
  assert.equal(request.url,"/spaces/work-notes/messages/conversation");
  assert.equal(request.options.method,"POST");
  assert.equal(request.options.credentials,"same-origin");
  assert.equal(request.options.redirect,"error");
  assert.deepEqual(JSON.parse(request.options.body),payload);
  assert.equal(request.options.headers.Authorization,undefined);
  assert.equal(JSON.stringify([...app.storage]).includes("fictional-private-session"),false);
  app.window.chromieNavigate("work-notes",false,false,false,true);
  assert.equal(app.address.pathname,"/spaces/work-notes/messages");
});

test("切会话和离开页面取消采集读取，迟到正文不能返回界面", async () => {
  let complete;
  const app=await boot({transport:()=>new Promise(resolve=>{complete=resolve;})});
  const pending=app.window.chromieMessages("work-notes","detail",JSON.stringify({event_id:"01a187e6-7324-768f-a294-bd316a236099"}),1);
  app.window.chromieCancelMessages();
  assert.equal(app.requests.at(-1).options.signal.aborted,true);
  complete({ok:true,status:200,text:async()=>"fictional late body"});
  await pending;
  assert.equal(app.messageResponses.length,0);
  const second=app.window.chromieMessages("work-notes","sessions","{}",2);
  app.events.get("pagehide")();
  complete({ok:true,status:200,text:async()=>"fictional late list"});
  await second;
  assert.equal(app.messageResponses.length,0);
});

test("采集浏览拒绝任意路径，失败时不读取响应原文", async () => {
  let read=false;
  const app=await boot({transport:async()=>({ok:false,status:401,text:async()=>{read=true;return "must not read";}})});
  await app.window.chromieMessages("work-notes","../../secrets","{}",1);
  assert.equal(app.requests.length,0);
  await app.window.chromieMessages("work-notes","sessions","{}",2);
  assert.equal(read,false);
  assert.deepEqual(app.messageResponses.at(-1),[2,401,""]);
});


test("Secret 主动复制复用 HTTP 回退，仅返回状态且立即清空临时 DOM", async () => {
  const app = await boot();
  assert.equal(app.clipboard.length, 0);
  delete app.navigator.clipboard;
  let selected, removed = false;
  const field = { value: "", style: {}, select() { selected = this.value; }, remove() { removed = true; } };
  app.pageDocument.createElement = () => field;
  app.pageDocument.body = { append() {} };
  app.pageDocument.execCommand = () => true;
  const requests = app.requests.length;
  await app.window.chromieCopySecret("{\"password\":\"fictional-only\"}", 11);
  assert.equal(selected, "{\"password\":\"fictional-only\"}");
  assert.equal(field.value, "");
  assert.equal(removed, true);
  assert.deepEqual(app.secretCopyNotices, [[11, true]]);
  assert.equal(app.requests.length, requests);
  assert.equal(app.storage.size, 0);
});

test("Secret 复制拒绝与无效正文不误报成功", async () => {
  const app = await boot();
  app.navigator.clipboard.writeText = async () => { throw new Error("虚构拒绝"); };
  const field = { value: "", style: {}, select() {}, remove() {} };
  app.pageDocument.createElement = () => field;
  app.pageDocument.body = { append() {} };
  app.pageDocument.execCommand = () => false;
  await app.window.chromieCopySecret("fictional-only", 12);
  await app.window.chromieCopySecret("", 13);
  assert.deepEqual(app.secretCopyNotices, [[12, false], [13, false]]);
  assert.equal(field.value, "");
});

test("普通文字指针在离开画布、换页与退出时恢复，不改变选择和请求", async () => {
  const app = await boot();
  const requests = app.requests.length;
  app.window.chromieArrowPointer(true);
  assert.equal(app.canvas.dataset.readingPointer, "true");
  app.events.get("pointerout")({ target: {} });
  assert.equal(app.canvas.dataset.readingPointer, "true");
  app.events.get("pointerout")({ target: app.canvas });
  assert.equal(app.canvas.dataset.readingPointer, "false");
  app.window.chromieArrowPointer(true);
  app.window.chromieNavigate("demo", true, false);
  assert.equal(app.canvas.dataset.readingPointer, "false");
  app.window.chromieArrowPointer(true);
  app.window.chromieCancel();
  assert.equal(app.canvas.dataset.readingPointer, "false");
  assert.equal(app.requests.length, requests);
  assert.equal(app.clipboard.length, 0);
});

test("看板排序只传固定枚举，默认请求兼容，非法值不发请求", async () => {
  const app = await boot();
  await app.window.chromieRequest("demo", "snapshot", "", 71, false, "popular");
  assert.equal(app.requests.at(-1).url, "/spaces/demo/dashboard-data?order=popular");
  await app.window.chromieRequest("demo", "snapshot", "", 72, false, "useful");
  assert.equal(app.requests.at(-1).url, "/spaces/demo/dashboard-data?order=useful");
  await app.window.chromieRequest("demo", "snapshot", "", 73);
  assert.equal(app.requests.at(-1).url, "/spaces/demo/dashboard-data");
  const count = app.requests.length;
  await app.window.chromieRequest("demo", "snapshot", "", 74, false, "other&query=private");
  assert.equal(app.requests.length, count);
  assert.deepEqual(app.responses.at(-1), [74, 400, "", false]);
});

test("Secret 格式化缩进可超过原文字节上限，复制仍有确定上界", async () => {
  const app = await boot();
  const pretty = "{\n" + '                "fictional": 1,\n'.repeat(3000) + "}";
  assert.ok(new TextEncoder().encode(pretty).length > 60000);
  await app.window.chromieCopySecret(pretty, 31);
  assert.deepEqual(app.secretCopyNotices, [[31, true]]);
  assert.equal(app.clipboard.at(-1), pretty);
  await app.window.chromieCopySecret("x".repeat(60000 * 18 + 1), 32);
  assert.deepEqual(app.secretCopyNotices.at(-1), [32, false]);
  assert.equal(app.clipboard.length, 1);
});


test("只读画布的原生复制只交付当前选区，空选区和编辑输入框不受影响", async () => {
  const app = await boot();
  const requests = app.requests.length;
  const copied = []; let prevented = 0; let stopped = 0;
  const event = { target: app.canvas, clipboardData: { setData: (type, text) => copied.push([type, text]) }, preventDefault: () => prevented++, stopPropagation: () => stopped++ };
  app.events.get("copy")(event);
  assert.equal(prevented, 0);
  assert.equal(stopped, 0);
  app.setSelection("中文🙂\n虚构选区");
  app.events.get("copy")(event);
  assert.deepEqual(copied, [["text/plain", "中文🙂\n虚构选区"]]);
  assert.equal(prevented, 1);
  assert.equal(stopped, 1);
  app.events.get("copy")({ ...event, target: {tagName:"INPUT"} });
  app.events.get("copy")({ ...event, clipboardData: null });
  assert.equal(prevented, 1);
  assert.equal(stopped, 1);
  assert.equal(app.requests.length, requests);
  assert.equal(app.storage.size, 0);
});
