import assert from "node:assert/strict";
import { request as httpRequest } from "node:http";

// Run against an already running desktop app. Fixtures use separate test accounts.
const root = "http://127.0.0.1:38261";
const request = (path, options = {}) =>
  fetch(root + path, { signal: AbortSignal.timeout(5000), ...options });
const pair = await (await request("/v1/pair")).json();
assert.match(pair.code, /^[0-9a-f]{32}$/);
const headers = {
  authorization: `Bearer ${pair.code}`,
  "content-type": "application/json",
};
assert.equal((await request("/v1/status")).status, 401);
assert.equal(
  (await request("/v1/pair", { headers: { origin: "https://example.com" } }))
    .status,
  403,
);
const hostStatus = await new Promise((resolve, reject) => {
  const req = httpRequest(
    root + "/v1/pair",
    { headers: { host: "example.com" } },
    (response) => {
      response.resume();
      resolve(response.statusCode);
    },
  );
  req.on("error", reject);
  req.end();
});
assert.equal(hostStatus, 403);
assert.equal(
  (await request("/v1/pair", { headers: { "sec-fetch-site": "cross-site" } }))
    .status,
  403,
);
assert.equal((await request("/v1/status", { headers })).status, 200);
assert.equal(
  (
    await request("/v1/names/7", {
      method: "POST",
      headers,
      body: "x".repeat(262145),
    })
  ).status,
  413,
);
assert.equal(
  (
    await request("/v1/names/7", {
      method: "POST",
      headers,
      body: JSON.stringify({
        version: 1,
        userId: "8",
        enabled: true,
        name: "Nope",
      }),
    })
  ).status,
  400,
);

function connect(userId, role) {
  const socket = new WebSocket("ws://127.0.0.1:38261/v1/socket");
  const received = [];
  socket.addEventListener("message", (event) =>
    received.push(JSON.parse(event.data)),
  );
  socket.addEventListener("open", () =>
    socket.send(
      JSON.stringify({
        type: "auth",
        version: 1,
        token: pair.code,
        userId,
        role,
      }),
    ),
  );
  return { socket, received };
}
async function until(predicate) {
  const end = Date.now() + 5000;
  while (!predicate()) {
    assert.ok(Date.now() < end, "Timed out waiting for the bridge");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
const game = connect("7", "game"),
  browser = connect("7", "browser"),
  other = connect("8", "game");
try {
  await until(() =>
    [game, browser, other].every((client) =>
      client.received.some((value) => value.type === "ready"),
    ),
  );
  const marker = `Test_${Date.now().toString().slice(-8)}`;
  browser.socket.send(
    JSON.stringify({
      type: "publish",
      channel: "names",
      value: { version: 1, userId: "7", enabled: true, name: marker },
    }),
  );
  await until(() =>
    game.received.some(
      (value) => value.channel === "names" && value.value.name === marker,
    ),
  );
  await until(() =>
    browser.received.some(
      (value) => value.type === "ack" && value.channel === "names",
    ),
  );
  assert.ok(
    !other.received.some(
      (value) => value.channel === "names" && value.value.name === marker,
    ),
  );
  game.socket.send(
    JSON.stringify({
      type: "publish",
      channel: "names",
      value: { version: 1, userId: "7", enabled: true, name: "Rejected" },
    }),
  );
  await until(() => game.received.some((value) => value.type === "error"));
  assert.equal(
    (await (await request("/v1/names/7", { headers })).json()).name,
    marker,
  );
  const outfit = {version:1,userId:"7",items:[],backgroundId:42,backgrounds:[{id:42,name:"Test background"}],definition:{
    assets:[],bodyColors:Object.fromEntries(["head","torso","leftArm","rightArm","leftLeg","rightLeg"].map(part=>[part+"Color","ffffff"])),
    scales:{},playerAvatarType:{playerAvatarType:"R15"}}};
  for (const backgroundId of [42,0]) {
    outfit.backgroundId=backgroundId;
    const response=await request("/v1/browser/7",{method:"POST",headers,body:JSON.stringify(outfit)});
    assert.equal(response.status,200);
    await until(()=>game.received.some(message=>message.channel==="browser" && message.value.backgroundId===backgroundId));
    const pushed=game.received.find(message=>message.channel==="browser" && message.value.backgroundId===backgroundId).value;
    assert.deepEqual(pushed.backgrounds,outfit.backgrounds);
    assert.equal(pushed.definition.backgroundRequestModel?.id,backgroundId || undefined);
    game.socket.send(JSON.stringify({type:"publish",channel:"game",value:{...outfit,definition:pushed.definition}}));
    await until(()=>browser.received.some(message=>message.channel==="game" && message.value.backgroundId===backgroundId));
    assert.equal((await (await request("/v1/game/7",{headers})).json()).backgroundId,backgroundId);
  }
  console.log(
    "Passed: pairing, guards, account isolation, WebSocket delivery, HTTP reads, and background selection/removal round trips.",
  );
} finally {
  for (const client of [game, browser, other]) client.socket.close();
}
