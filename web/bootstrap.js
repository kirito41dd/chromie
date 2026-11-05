import init, { receive_transfer, receive_transfer_file, set_transfer_page, receive_model, set_settings_page, selected_text, receive_messages, set_messages_page, receive_access, receive_access_notice, hide_access_keys, resume_access, set_access_page, start, set_page, receive_snapshot, receive_recall, receive_secret, receive_secret_copy, receive_secret_verification, hide_secret, end_session, resize_viewport, restore_session } from "/web/chromie_web.js";

let activeRequest;
let activeRecall;
let activeAccess;
let activeSecret;
let secretHideTimer;
let secretVerification;
let secretVerificationTimer;
let secretVerificationVersion = 0;
let ready = false;
let resizeTimer;
let layoutWidth;
let activeSpace = "";
let savedVersion = 0;
const savedLoginKey = "chromie.saved-login";

// Slint 仍处理选择与原生指针；仅把普通文字的 text 指针覆盖为箭头。
window.chromieArrowPointer = arrow => {
  const canvas = document.getElementById("canvas");
  if (canvas) canvas.dataset.readingPointer = arrow ? "true" : "false";
};
document.addEventListener("pointerout", event => {
  if (event.target === document.getElementById("canvas")) window.chromieArrowPointer(false);
}, true);

// IME 首键可能先于 compositionstart：isComposing 仍为 false，但 keyCode 已是 229。
// Slint 1.18.1 只检查 isComposing；补上 229 边界，避免把首字母提前写入再叠加拼音预编辑。
// 只拦截发往 Slint 画布／辅助 input 的组合按键；不取消默认行为，组合和 input 事件继续交给 SDK。
function guardImeKey(event) {
  const canvas = document.getElementById("canvas");
  const target = event.target;
  const slintInput = target?.tagName === "INPUT" &&
    target.parentElement === document.getElementById("viewport") && target.nextElementSibling === canvas;
  if (canvas && (target === canvas || slintInput) && (event.isComposing || event.keyCode === 229)) {
    event.stopPropagation();
  }
}
document.addEventListener("keydown", guardImeKey, true);
document.addEventListener("keyup", guardImeKey, true);

function isSlintCopyTarget(target) {
  const canvas = document.getElementById("canvas");
  return !!canvas && (target === canvas || (target?.tagName === "INPUT" &&
    target.parentElement === document.getElementById("viewport") && target.nextElementSibling === canvas));
}

// Winit 默认取消画布 keydown，浏览器可能不再派发 copy。仅在只读选区存在时
// 从捕获阶段完成用户的复制手势；无选区、编辑框和其他 DOM 控件沿用原生行为。
document.addEventListener("keydown", event => {
  if (!ready || event.isComposing || event.keyCode === 229 || event.altKey || event.shiftKey ||
      !(event.metaKey || event.ctrlKey) || event.key?.toLowerCase() !== "c" || !isSlintCopyTarget(event.target)) return;
  const text = selected_text();
  if (!text) return;
  event.preventDefault();
  event.stopPropagation();
  void copyText(text);
}, true);

// 菜单“复制”等原生入口也可读取选区；不将正文常驻缓存到 JS。
document.addEventListener("copy", event => {
  if (!ready || !isSlintCopyTarget(event.target) || !event.clipboardData) return;
  const text = selected_text();
  if (!text) return;
  event.clipboardData.setData("text/plain", text);
  event.preventDefault();
  event.stopPropagation();
}, true);

// 容器保持像素尺寸，停止拖动 200ms 后再同时更新画布盒子与布局断点。
// Winit 继续负责物理像素和 DPI；应用不直接重设 canvas 位图或 Slint 窗口。
function fitViewport() {
  const width = window.innerWidth;
  const height = window.innerHeight;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return;
  const viewport = document.getElementById("viewport");
  if (viewport.style.width !== `${width}px`) viewport.style.width = `${width}px`;
  if (viewport.style.height !== `${height}px`) viewport.style.height = `${height}px`;
  if (ready && width !== layoutWidth) {
    resize_viewport(width);
    layoutWidth = width;
  }
}

// Fetch 的 Header 值使用 ByteString，将 UTF-8 字节保留为单字节字符。
// 登录和独立 Secret 验密使用密码请求头；普通看板请求使用 Cookie。
function passwordHeader(password) {
  return String.fromCharCode(...new TextEncoder().encode(password));
}

function rememberSpace(space) {
  try { window.localStorage.setItem("chromie.space", space); } catch { /* 存储受限时仍可通过空间地址恢复。 */ }
}

function lastSpace() {
  try { return window.localStorage.getItem("chromie.space") || savedLogin()?.space || ""; } catch { return ""; }
}

function pageRoute() {
  if (location.pathname === "/") return { space: lastSpace(), recall: false };
  const match = location.pathname.match(/^\/spaces\/([^/]+)\/(dashboard|recall|access|messages|settings|transfer)$/);
  if (!match) return;
  try { return { space: decodeURIComponent(match[1]), recall: match[2] === "recall", access: match[2] === "access", messages: match[2] === "messages", settings: match[2] === "settings", transfer: match[2] === "transfer" }; } catch { /* 无效地址不复用旧空间。 */ }
}

function pageTitle(recall, access = false, messages = false, settings = false, transfer = false) {
  document.title = transfer ? "Chromie · 炼化／夺舍" : settings ? "Chromie · 系统设置" : messages ? "Chromie · 采集消息" : access ? "Chromie · 接入" : recall ? "Chromie · 召回调试" : "Chromie · 看板";
}

window.chromieNavigate = (space, recall, replace, access = false, messages = false, settings = false, transfer = false) => {
  if (typeof space !== "string" || space.length > 32 || !/^[a-z]+(?:-[a-z]+)*$/.test(space)) return;
  const path = `/spaces/${encodeURIComponent(space)}/${transfer ? "transfer" : settings ? "settings" : messages ? "messages" : access ? "access" : recall ? "recall" : "dashboard"}`;
  if (secretVerification && secretVerification.space !== space) window.chromieForgetSecretVerification();
  activeSpace = space;
  if (location.pathname !== path) {
    window.chromieArrowPointer(false);
    if (ready) { hide_secret(); hide_access_keys(); }
    window.chromieCancelSecret();
    window.chromieCancelMessages();
    if (replace) window.history.replaceState(null, "", path);
    else window.history.pushState(null, "", path);
  }
  pageTitle(recall, access, messages, settings, transfer);
};

function savedLogin() {
  try {
    const value = JSON.parse(window.localStorage.getItem(savedLoginKey));
    if (typeof value?.space !== "string" || value.space.length > 32 ||
        !/^[a-z]+(?:-[a-z]+)*$/.test(value.space) ||
        typeof value.password !== "string" || !value.password || value.password.length > 1024) return;
    return value;
  } catch { /* 记录损坏或存储受限时继续手动登录。 */ }
}

window.chromieSavedPassword = space => {
  const saved = savedLogin();
  return saved?.space === space ? saved.password : "";
};
window.chromieForgetPassword = () => {
  savedVersion += 1;
  try { window.localStorage.removeItem(savedLoginKey); return true; } catch { return false; }
};
window.chromieCancel = () => {
  window.chromieArrowPointer(false);
  const pending = activeRequest;
  activeRequest = undefined;
  pending?.abort();
  window.chromieCancelRecall();
  window.chromieCancelAccess();
  window.chromieCancelMessages?.();
  window.chromieCancelModel?.();
  window.chromieCancelTransfer?.();
  window.chromieCancelSecret();
  window.chromieForgetSecretVerification();
};
window.chromieCancelRecall = () => {
  const pending = activeRecall;
  activeRecall = undefined;
  pending?.abort();
};

// 验密密码只留在当前标签页内存；固定五分钟，不随查看续期，也不复用已保存的登录密码。
window.chromieSecretVerified = space => !!secretVerification &&
  secretVerification.space === space && performance.now() < secretVerification.expiresAt;
window.chromieForgetSecretVerification = () => {
  secretVerificationVersion += 1;
  clearTimeout(secretVerificationTimer);
  secretVerificationTimer = undefined;
  secretVerification = undefined;
  if (ready) receive_secret_verification(false);
};
function rememberSecretVerification(space, password) {
  window.chromieForgetSecretVerification();
  secretVerification = { space, password, expiresAt: performance.now() + 300000 };
  receive_secret_verification(true);
  secretVerificationTimer = setTimeout(() => {
    window.chromieForgetSecretVerification();
    hide_secret();
    window.chromieCancelSecret();
  }, 300000);
}

// Secret 原文只送到独立浮层，不进入普通快照、召回诊断或浏览器存储。
window.chromieCancelSecret = () => {
  clearTimeout(secretHideTimer);
  secretHideTimer = undefined;
  const pending = activeSecret;
  activeSecret = undefined;
  pending?.abort();
};
window.chromieRevealSecret = async (space, memoryId, password, generation) => {
  window.chromieCancelSecret();
  const reused = password === "" && window.chromieSecretVerified(space);
  if (reused) password = secretVerification.password;
  if (typeof space !== "string" || space.length > 32 || !/^[a-z]+(?:-[a-z]+)*$/.test(space) ||
      typeof memoryId !== "string" || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(memoryId) ||
      typeof password !== "string" || !password || new TextEncoder().encode(password).length > 1024 ||
      /[\u0000-\u001f\u007f-\u009f]/.test(password)) {
    receive_secret(generation, 400, "", false);
    return;
  }
  const controller = new AbortController();
  const verificationVersion = secretVerificationVersion;
  activeSecret = controller;
  const timeout = setTimeout(() => controller.abort(), 10000);
  let text = "";
  let bytes;
  try {
    const response = await fetch(`/spaces/${encodeURIComponent(space)}/secrets/${encodeURIComponent(memoryId)}`, {
      method: "GET", headers: { "X-Chromie-Password": passwordHeader(password) },
      cache: "no-store", credentials: "omit", redirect: "error", mode: "same-origin", signal: controller.signal,
    });
    if (activeSecret !== controller) return;
    if (controller.signal.aborted) throw new Error("cancelled");
    if (!response.ok) {
      if (response.status === 401) window.chromieForgetSecretVerification();
      receive_secret(generation, response.status, "", false);
      return;
    }
    if (response.headers.get("content-type")?.split(";", 1)[0].trim().toLowerCase() !== "application/octet-stream") {
      receive_secret(generation, response.status, "", true);
      return;
    }
    bytes = new Uint8Array(await response.arrayBuffer());
    if (activeSecret !== controller) return;
    if (controller.signal.aborted) throw new Error("cancelled");
    if (reused && (!window.chromieSecretVerified(space) || verificationVersion !== secretVerificationVersion)) {
      window.chromieForgetSecretVerification();
      receive_secret(generation, 401, "", false);
      return;
    }
    let unsupported = bytes.length === 0 || bytes.length > 60000;
    if (!unsupported) {
      try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
      catch { unsupported = true; }
      unsupported ||= /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(text);
    }
    bytes.fill(0);
    if (unsupported) { receive_secret(generation, response.status, "", true); return; }
    if (receive_secret(generation, response.status, text, false)) {
      if (!reused && verificationVersion === secretVerificationVersion) rememberSecretVerification(space, password);
      secretHideTimer = setTimeout(() => {
        secretHideTimer = undefined;
        hide_secret();
      }, 60000);
    }
  } catch {
    if (activeSecret === controller) receive_secret(generation, 0, "", false);
  } finally {
    password = "";
    text = "";
    bytes?.fill(0);
    clearTimeout(timeout);
    if (activeSecret === controller) activeSecret = undefined;
  }
};

// 召回使用独立通道，不与看板轮询相互取消；查询仅放在请求正文中。
window.chromieRecall = async (space, query, generation) => {
  window.chromieCancelRecall();
  const controller = new AbortController();
  activeRecall = controller;
  const timeout = setTimeout(() => controller.abort(), 10000);
  const started = performance.now();
  const requestedAt = new Date().toISOString();
  const complete = (status, body) => {
    if (activeRecall === controller) {
      receive_recall(generation, status, body, Math.max(0, Math.round(performance.now() - started)), requestedAt);
    }
  };
  try {
    const response = await fetch(`/spaces/${encodeURIComponent(space)}/recall`, {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ query, max_chars: 8000 }),
      cache: "no-store", credentials: "same-origin", redirect: "error", mode: "same-origin",
      signal: controller.signal,
    });
    const body = await response.text();
    complete(response.status, body);
  } catch {
    complete(0, "");
  } finally {
    clearTimeout(timeout);
    if (activeRecall === controller) activeRecall = undefined;
  }
};
// 原始采集浏览使用独立、有界读取；正文与筛选不写入 URL 或存储。
let activeMessages;
window.chromieCancelMessages = () => { activeMessages?.abort(); activeMessages = undefined; };
window.chromieMessages = async (space, operation, payload, generation) => {
  window.chromieCancelMessages();
  if (typeof space !== "string" || !/^[a-z]+(?:-[a-z]+)*$/.test(space) || space.length > 32) return;
  let data;
  try { data = JSON.parse(payload); } catch { receive_messages(generation, 400, ""); return; }
  let path, method = "POST";
  if (operation === "agents") { path = "agents"; method = "GET"; }
  else if (operation === "sessions") path = "messages/sessions";
  else if (operation === "conversation") path = "messages/conversation";
  else if (operation === "detail" && typeof data.event_id === "string" && /^[0-9a-f-]{36}$/i.test(data.event_id)) { path = `messages/events/${data.event_id}`; method = "GET"; }
  else { receive_messages(generation, 400, ""); return; }
  const controller = new AbortController(); activeMessages = controller;
  const timeout = setTimeout(() => controller.abort(), 10000);
  try {
    const response = await fetch(`/spaces/${encodeURIComponent(space)}/${path}`, {
      method, ...(method === "POST" ? {headers: {"Content-Type": "application/json"}, body: JSON.stringify(data)} : {}),
      cache: "no-store", credentials: "same-origin", redirect: "error", mode: "same-origin", signal: controller.signal,
    });
    // 最大正文 60,000 字符，UTF-8 或 JSON 转义后也须保持有限响应。
    const body = response.ok ? await response.text() : "";
    if (activeMessages === controller) receive_messages(generation, body.length > 400000 ? 502 : response.status, body.length > 400000 ? "" : body);
  } catch { if (activeMessages === controller) receive_messages(generation, 0, ""); }
  finally { clearTimeout(timeout); if (activeMessages === controller) activeMessages = undefined; }
};
window.chromieUsesNushell = () => {try {return window.localStorage.getItem("chromie.shell") === "nushell";} catch {return false;}};
window.chromieSetShell = nushell => {try {window.localStorage.setItem("chromie.shell",nushell ? "nushell" : "bash");} catch { /* 偏好保存失败不影响当前选择。 */ }};
window.chromieOrigin = () => location.origin;
// 配对 URL 只带公开请求 ID，不含领取凭据或 Agent 密钥。
const pairingSpace = pageRoute()?.space;
window.chromiePairingId = () => {
  const route = pageRoute();
  if (!route?.access || route.space !== pairingSpace) return "";
  return (location.search || "").match(/(?:^\?|&)pair=([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})(?:&|$)/)?.[1] || "";
};
window.chromieDismissPairing = () => {
  const route = pageRoute();
  if (route?.access) window.history.replaceState(null, "", location.pathname);
};
window.chromieNow = () => performance.now();
window.chromieCancelAccess = () => {
  const pending = activeAccess;
  activeAccess = undefined;
  pending?.abort();
};
async function copyText(text) {
  try {
    if (!navigator.clipboard?.writeText) throw new Error("当前页面使用传统复制接口");
    await navigator.clipboard.writeText(text);
    return true;
  } catch { /* HTTP 页面可能没有 Clipboard API，沿用本次点击的同步复制能力。 */ }
  const previous = document.activeElement;
  let field;
  let copied = false;
  try {
    field = document.createElement("textarea");
    field.value = text;
    field.readOnly = true;
    field.style.position = "fixed";
    field.style.left = "-10000px";
    document.body.append(field);
    field.select();
    copied = document.execCommand("copy");
  } catch { /* 只返回状态，不回显待复制内容。 */ }
  finally {
    if (field) { field.value = ""; field.remove(); }
    previous?.focus?.({ preventScroll: true });
  }
  return copied;
}
window.chromieCopyAccess = async (text, generation) => {
  receive_access_notice(generation, await copyText(text));
};
window.chromieCopySecret = async (text, generation) => {
  // 原文上限 60 KB；格式化每个结构字符最多增加换行与 16 个缩进空格。
  if (typeof text !== "string" || !text || new TextEncoder().encode(text).length > 60000 * 18) {
    receive_secret_copy(generation, false); return;
  }
  receive_secret_copy(generation, await copyText(text));
};
window.chromieAccess = async (space, operation, id, kind, payload, generation) => {
  window.chromieCancelAccess();
  const controller = new AbortController();
  activeAccess = controller;
  const timeout = setTimeout(() => controller.abort(), 10000);
  const validSpace = typeof space === "string" && space.length <= 32 && /^[a-z]+(?:-[a-z]+)*$/.test(space);
  const validId = typeof id === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(id);
  const validKind = kind === "mcp" || kind === "hook";
  try {
    if (!validSpace || !["list", "create", "update", "reveal", "rotate", "download", "pair-info", "pair-approve"].includes(operation) ||
        (!["list", "create"].includes(operation) && !validId) ||
        (["reveal", "rotate"].includes(operation) && !validKind)) {
      receive_access(generation, 400, ""); return;
    }
    const prefix = `/spaces/${encodeURIComponent(space)}/agents`;
    const options = { cache: "no-store", credentials: "same-origin", redirect: "error", mode: "same-origin", signal: controller.signal };
    if (operation === "download") {
      const keys = {};
      for (const purpose of ["mcp", "hook"]) {
        const response = await fetch(`${prefix}/${id}/keys/${purpose}/reveal`, { ...options, method: "POST" });
        if (activeAccess !== controller) return;
        if (!response.ok) { receive_access(generation, response.status, ""); return; }
        const value = await response.json();
        if (activeAccess !== controller) return;
        if (typeof value.key !== "string" || !new RegExp(`^cm_${purpose}_[0-9a-f]{64}$`).test(value.key)) throw new Error("接入密钥格式无效");
        keys[`${purpose}_key`] = value.key;
      }
      if (activeAccess !== controller || document.hidden) return;
      const content = JSON.stringify({ url: location.origin, space, agent_id: id, ...keys }, null, 2);
      const url = URL.createObjectURL(new Blob([content], { type: "application/json" }));
      try {
        const link = document.createElement("a"); link.href = url; link.download = `chromie-${id}.json`; link.click();
      } finally { URL.revokeObjectURL(url); }
      receive_access(generation, 200, '{"downloaded":true}');
      return;
    }
    const pairing = operation === "pair-info" || operation === "pair-approve";
    const path = pairing ? `${prefix}/pairings/${id}` : operation === "list" || operation === "create" ? prefix :
      operation === "update" ? `${prefix}/${id}` : `${prefix}/${id}/keys/${kind}/${operation}`;
    const method = operation === "list" || operation === "pair-info" ? "GET" : operation === "update" ? "PATCH" : "POST";
    const body = operation === "create" || operation === "update" || operation === "pair-approve" ? payload : undefined;
    const response = await fetch(path, { ...options, method, ...(body === undefined ? {} : { headers: { "Content-Type": "application/json" }, body }) });
    if (activeAccess !== controller) return;
    const text = response.ok ? await response.text() : "";
    if (activeAccess === controller) receive_access(generation, response.status, text);
  } catch { if (activeAccess === controller) receive_access(generation, 0, ""); }
  finally { clearTimeout(timeout); if (activeAccess === controller) activeAccess = undefined; }
};
let activeModel;
window.chromieCancelModel = () => {
  const pending = activeModel;
  activeModel = undefined;
  pending?.abort();
};
window.chromieModelRequest = async (space, operation, payload, generation) => {
  if (!["read", "test", "save", "disable", "retry"].includes(operation) || !/^[a-z]+(?:-[a-z]+)*$/.test(space)) {
    receive_model(generation, 400, ""); return;
  }
  const controller = new AbortController();
  window.chromieCancelModel();
  activeModel = controller;
  const timeout = setTimeout(() => controller.abort(), 20000);
  try {
    const suffix = ["test", "disable", "retry"].includes(operation) ? `/${operation}` : "";
    const response = await fetch(`/spaces/${encodeURIComponent(space)}/system/model${suffix}`, {
      method: operation === "read" ? "GET" : "POST", credentials: "same-origin", mode: "same-origin", redirect: "error", cache: "no-store", signal: controller.signal,
      ...(operation === "read" ? {} : {headers: {"Content-Type": "application/json"}, body: payload}),
    });
    payload = "";
    const body = await response.text();
    if (activeModel === controller) receive_model(generation, response.status, body);
  } catch {
    if (activeModel === controller) receive_model(generation, 0, "");
  } finally {
    payload = "";
    clearTimeout(timeout);
    if (activeModel === controller) activeModel = undefined;
  }
};

// 浏览器桥接仅负责文件选择、二进制下载和请求；Secret 不进入页面模型。
let transferFile;
let transferInput;
let activeTransfer;
window.chromieCancelTransfer = () => {
  const pending = activeTransfer; activeTransfer = undefined; pending?.abort();
  transferFile = undefined;
  if (transferInput) { transferInput.onchange = null; transferInput.remove(); transferInput = undefined; }
};
window.chromieChooseTransfer = () => {
  transferInput?.remove();
  const input = document.createElement("input");
  transferInput = input; input.type = "file"; input.accept = ".chromie,.age"; input.hidden = true;
  input.onchange = () => {
    const file = input.files?.[0];
    if (transferInput !== input || !pageRoute()?.transfer || !file) return;
    transferFile = file.size > 0 && file.size <= 65 * 1024 * 1024 ? file : undefined;
    receive_transfer_file(file.name, file.size);
    input.value = "";
  };
  document.body.append(input); input.click();
};
window.chromieTransferRequest = async (space, operation, payload, generation) => {
  if (!["unlock-status", "unlock", "lock", "export", "preview", "confirm"].includes(operation) || !/^[a-z]+(?:-[a-z]+)*$/.test(space)) {
    receive_transfer(generation, 400, ""); return;
  }
  activeTransfer?.abort();
  const controller = new AbortController(); activeTransfer = controller;
  const timeout = setTimeout(() => controller.abort(), 120000);
  let body;
  try {
    const parsed = JSON.parse(payload); payload = "";
    let headers;
    if (operation === "unlock-status" || operation === "lock") {
      body = undefined;
    } else if (operation === "unlock") {
      body = JSON.stringify({space_password: parsed.space_password});
      headers = {"Content-Type": "application/json"};
    } else if (operation === "export") {
      body = JSON.stringify({passphrase: parsed.passphrase});
      headers = {"Content-Type": "application/json"};
    } else {
      if (!transferFile) { receive_transfer(generation, 400, ""); return; }
      body = new FormData();
      body.append("passphrase", parsed.passphrase);
      body.append("package", transferFile);
      if (operation === "preview") {
        if (!Number.isInteger(parsed.page) || parsed.page < 0 || parsed.page >= 2000) throw new Error("预览页码无效");
        body.append("page", String(parsed.page));
      }
      if (parsed.preview_digest) body.append("preview_digest", parsed.preview_digest);
    }
    parsed.space_password = ""; parsed.passphrase = "";
    const suffix = operation === "unlock-status" ? "unlock" : operation;
    const response = await fetch(`/spaces/${encodeURIComponent(space)}/transfer/${suffix}`, {
      method: operation === "unlock-status" ? "GET" : "POST", body, headers, credentials: "same-origin", mode: "same-origin", redirect: "error", cache: "no-store", signal: controller.signal,
    });
    body = undefined;
    if (activeTransfer !== controller) return;
    if (operation === "export" && response.ok) {
      const blob = await response.blob();
      if (activeTransfer !== controller) return;
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a"); anchor.href = url;
      const match = response.headers.get("Content-Disposition")?.match(/filename="([a-z0-9.-]+)"/);
      anchor.download = match?.[1] || "chromie-soul.chromie";
      document.body.append(anchor); anchor.click(); anchor.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      receive_transfer(generation, response.status, "{}");
    } else {
      const text = await response.text();
      if (activeTransfer === controller) receive_transfer(generation, response.status, text);
    }
  } catch {
    if (activeTransfer === controller) receive_transfer(generation, 0, "");
  } finally {
    payload = ""; body = undefined; clearTimeout(timeout);
    if (activeTransfer === controller) activeTransfer = undefined;
  }
};

window.chromieReady = () => {
  ready = true;
  fitViewport();
  document.getElementById("loading").hidden = true;
};
window.chromieRequest = async (space, operation, password, generation, remember = false, order = "recent") => {
  if (!["recent", "popular", "useful"].includes(order)) { receive_snapshot(generation, 400, "", false); return; }
  if (operation === "logout" || operation === "login") window.chromieForgetSecretVerification();
  if (operation === "logout") { if (ready) hide_secret(); window.chromieCancelSecret(); }
  const controller = new AbortController();
  activeRequest?.abort();
  activeRequest = controller;
  const timeout = setTimeout(() => controller.abort(), 10000);
  const version = savedVersion;
  let storageFailed = false;
  try {
    const prefix = `/spaces/${encodeURIComponent(space)}`;
    const options = {
      cache: "no-store", credentials: "same-origin", redirect: "error", mode: "same-origin",
      signal: controller.signal,
    };
    if (operation === "login") {
      if (!remember) storageFailed = !window.chromieForgetPassword();
      const headers = { "X-Chromie-Password": passwordHeader(password) };
      const login = await fetch(`${prefix}/login`, { ...options, method: "POST", headers });
      if (activeRequest !== controller) return;
      if (!login.ok) { receive_snapshot(generation, login.status, "", false); return; }
      // 只保存已通过验密的凭据，旧请求不得撤销用户的清除操作。
      if (remember && version === savedVersion) {
        try { window.localStorage.setItem(savedLoginKey, JSON.stringify({ space, password })); }
        catch { storageFailed = true; }
      }
      password = "";
    }
    const resource = operation === "logout" ? "logout" : `dashboard-data${order === "recent" ? "" : `?order=${order}`}`;
    const response = await fetch(`${prefix}/${resource}`, {
      ...options, method: operation === "logout" ? "POST" : "GET",
    });
    const body = response.ok && operation !== "logout" ? await response.text() : "";
    if (activeRequest !== controller) return;
    if (response.ok && operation !== "logout") rememberSpace(space);
    receive_snapshot(generation, response.status, body, storageFailed);
  } catch {
    if (activeRequest === controller) receive_snapshot(generation, 0, "", storageFailed);
  } finally {
    password = "";
    clearTimeout(timeout);
    if (activeRequest === controller) activeRequest = undefined;
  }
};

document.addEventListener("visibilitychange", () => {
  if (ready && document.hidden) { hide_secret(); hide_access_keys(); window.chromieCancelSecret(); window.chromieCancelAccess(); }
  else if (ready && pageRoute()?.access) resume_access();
});
window.addEventListener("pagehide", () => {
  window.chromieCancelMessages();
  clearTimeout(resizeTimer);
  resizeTimer = undefined;
  if (ready) end_session();
  window.chromieCancel();
});
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    resizeTimer = undefined;
    fitViewport();
  }, 200);
});
window.addEventListener("pageshow", event => {
  fitViewport();
  if (ready && event?.persisted) restore_session();
});
window.addEventListener("popstate", () => {
  window.chromieArrowPointer(false);
  if (!ready) return;
  const route = pageRoute();
  if (!route || route.space !== activeSpace) {
    end_session();
    window.chromieCancel();
    location.reload();
    return;
  }
  hide_secret();
  window.chromieCancelSecret();
  window.chromieCancelMessages();
  pageTitle(route.recall, route.access, route.messages, route.settings, route.transfer);
  if (route.transfer) set_transfer_page(true); else if (route.settings) set_settings_page(true); else if (route.messages) set_messages_page(true); else if (route.access) set_access_page(true); else set_page(route.recall);
});
try {
  fitViewport();
  await init();
  const route = pageRoute();
  if (!route) throw new Error("页面地址无效");
  activeSpace = route.space;
  pageTitle(route.recall, route.access, route.messages, route.settings, route.transfer);
  start(route.space, route.recall, !!route.access, !!route.messages, !!route.settings, !!route.transfer);
} catch (error) {
  // 当前 SDK 使用的 Winit 0.30 在 Web 上以此异常交接控制流；其他异常仍显示失败。
  const eventLoopHandoff = ready && error instanceof Error && error.message ===
    "Using exceptions for control flow, don't mind me. This isn't actually an error!";
  if (!eventLoopHandoff) {
    ready = false;
    document.getElementById("loading").hidden = false;
    document.getElementById("loading-message").textContent = "看板加载失败，请检查 Web 构建产物后刷新。";
  }
}
