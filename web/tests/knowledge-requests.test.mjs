// 保护公开浏览器桥接的请求生命周期和接收预算；不启动浏览器、服务或真实模型。
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";

function bridge() {
  const source = fs.readFileSync(new URL("../bootstrap.js", import.meta.url), "utf8");
  const start = source.indexOf("window.chromieRequestId =");
  const end = source.indexOf("window.chromieUsesNushell", start);
  assert.ok(start >= 0 && end > start, "公开请求桥接入口必须存在");
  const requests = [], text = [], messages = [];
  const scope = { window: {}, AbortController, Uint8Array, setTimeout, clearTimeout,
    receive_text_model: (...args) => text.push(args), receive_messages: (...args) => messages.push(args), receive_knowledge: () => {},
    fetch: (path, options) => new Promise(resolve => requests.push({path, options, resolve})),
  };
  vm.runInNewContext(source.slice(start, end), scope);
  return { window: scope.window, requests, text, messages };
}
function reply(request, body) { request.resolve({status: 200, ok: true, text: async () => JSON.stringify(body)}); }

test("消息读取与后台任务查询独立完成，不能取消另一通道并丢失回调", async () => {
  const b = bridge();
  const jobs = b.window.chromieTextModel("demo", "jobs", "{}", 1);
  const messages = b.window.chromieMessages("demo", "agents", "{}", 1);
  const aborted = b.requests[0].options.signal.aborted;
  reply(b.requests[0], {enabled: true, jobs: []}); reply(b.requests[1], {agents: []});
  await Promise.all([jobs, messages]);
  assert.equal(aborted, false, "消息读取不能取消任务查询");
  assert.equal(b.text.length, 1, "任务查询必须回调释放 Rust 加载状态");
  assert.equal(b.text[0][2], 200); assert.equal(b.messages.length, 1);
});

test("正常和控制字符最坏转义的合法任务历史都能返回状态与完整冲突引用", async () => {
  for (const reason of ["x".repeat(500), "\0".repeat(2000)]) {
    const b = bridge();
    const body = {enabled: true, jobs: Array.from({length:20}, () => ({state:"done", result:{memory_ids:[], conflicts:Array.from({length:10}, () => ({reason, memory_ids:["01900000-0000-7000-8000-000000000000"]}))}}))};
    assert.ok(JSON.stringify(body).length > 100000);
    const read = b.window.chromieTextModel("demo", "jobs", "{}", 7);
    reply(b.requests[0], body); await read;
    assert.equal(b.text[0][2], 200, "后端允许的完整历史不能被前端整包丢弃");
    assert.equal(JSON.parse(b.text[0][3]).jobs[19].result.conflicts[9].reason, reason);
  }
});


test("候选详情与任务轮询独立，确认修订仍使用同源POST", async () => {
  const b=bridge();
  const review=b.window.chromieTextModel("demo","review",JSON.stringify({job_id:"01900000-0000-7000-8000-000000000000"}),1);
  const jobs=b.window.chromieTextModel("demo","jobs","{}",2);
  assert.equal(b.requests[0].options.signal.aborted,false);
  assert.equal(b.requests[0].path,"/spaces/demo/knowledge/review");
  assert.equal(b.requests[0].options.credentials,"same-origin");
  assert.equal(b.requests[0].options.method,"POST");
  reply(b.requests[0],{result:{decisions:[]}});reply(b.requests[1],{jobs:[]});await Promise.all([review,jobs]);
  const apply=b.window.chromieTextModel("demo","apply-revision",JSON.stringify({job_id:"01900000-0000-7000-8000-000000000000",candidate_index:0}),3);
  assert.equal(b.requests[2].path,"/spaces/demo/knowledge/apply-revision");
  reply(b.requests[2],{revision:2});await apply;
  assert.equal(b.text.at(-1)[0],"apply-revision");
});
