// 重载属于只读操作，不能把未保存的任务选择意外提交给后端。
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";

test("任务设置重载只读取已保存选择，不提交当前草稿", async () => {
  const source = fs.readFileSync(new URL("../bootstrap.js", import.meta.url), "utf8");
  const start = source.indexOf("const activeProviders =");
  const end = source.indexOf("\ntry {", start);
  assert.ok(start >= 0 && end > start);
  const requests = [], replies = [];
  const scope = {
    window: {}, AbortController, setTimeout, clearTimeout,
    receive_provider: (...args) => replies.push(args),
    fetch: async (path, options) => {
      requests.push({path, options});
      return {status: 200, text: async () => JSON.stringify({revision: 1, enabled: false})};
    },
  };
  vm.runInNewContext(source.slice(start, end), scope);
  for (const operation of ["organization-read", "organization-reload"]) {
    await scope.window.chromieProviderRequest("demo", operation, '{"enabled":true}', 7);
    const request = requests.at(-1);
    assert.equal(request.path, "/spaces/demo/system/organization");
    assert.equal(request.options.method, "GET");
    assert.equal(request.options.body, undefined);
    assert.equal(request.options.credentials, "same-origin");
    assert.equal(replies.at(-1)[0], operation);
    assert.equal(replies.at(-1)[2], 200);
  }
});
