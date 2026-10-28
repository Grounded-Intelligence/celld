import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { cp, mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import test from "node:test";

const binary = process.env.CELLD_BIN ?? fileURLToPath(new URL("../target/lab/celld", import.meta.url));

test("background continuations retain their WebSockets after the delivering event ends", { timeout: 60_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "celld-websocket-origin-"));
  let runtime;
  let stopped;
  t.after(async () => {
    if (runtime) {
      runtime.kill("SIGTERM");
      await stopped;
    }
    await rm(directory, { recursive: true, force: true });
  });
  await cp(new URL("fixtures/websocket-origin/", import.meta.url), directory, { recursive: true });

  const reservation = createServer();
  reservation.listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const { port } = reservation.address();
  await new Promise((resolve) => reservation.close(resolve));
  const base = `http://127.0.0.1:${port}`;
  runtime = spawn(binary, ["dev", directory, "--host", "127.0.0.1", "--port", String(port), "--logs"]);
  let logs = "";
  runtime.stdout.on("data", (chunk) => { logs += chunk; });
  runtime.stderr.on("data", (chunk) => { logs += chunk; });
  stopped = once(runtime, "exit");

  let ready = false;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (runtime.exitCode !== null) throw new Error(logs);
    try {
      ready = (await fetch(`${base}/health`, { signal: AbortSignal.timeout(500) })).ok;
    } catch {}
    if (ready) break;
    await delay(50);
  }
  assert.ok(ready, `runtime did not start: ${logs}`);

  await t.test("Worker response transfers socket ownership", async () => {
    const socket = new WebSocket(`${base.replace("http:", "ws:")}/worker-echo`);
    try {
      const echoed = await new Promise((resolve, reject) => {
        socket.onopen = () => socket.send("after handoff");
        socket.onmessage = (event) => resolve(event.data);
        socket.onclose = (event) => reject(new Error(`closed: ${event.code} ${event.reason}`));
      });
      assert.equal(echoed, "after handoff");
    } finally {
      socket.close();
    }
  });

  for (const mode of ["constructor", "upgrade", "binding"]) {
    await t.test(mode, async () => {
      const query = new URLSearchParams({ id: mode, mode });
      assert.equal((await fetch(`${base}/start?${query}`)).status, 200);
      let results = [];
      for (let attempt = 0; attempt < 200; attempt++) {
        results = await (await fetch(`${base}/results?${query}`, { signal: AbortSignal.timeout(1000) })).json();
        if (results.length === 20) break;
        await delay(25);
      }
      assert.deepEqual(results, Array.from({ length: 20 }, () => ({ echo: "command-check" })));
    });
  }
});
