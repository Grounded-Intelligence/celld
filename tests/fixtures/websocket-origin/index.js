async function exchange(url, env, mode) {
  let socket;
  if (mode === "constructor") {
    socket = new WebSocket(url.replace("http:", "ws:"));
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onclose = (event) => reject(new Error(`opening: ${event.code}`));
    });
  } else {
    const request = new Request(url, { headers: { Upgrade: "websocket" } });
    const response = mode === "upgrade"
      ? await fetch(request)
      : await env.ECHO.getByName("echo").fetch(request);
    socket = response.webSocket;
    socket.accept();
  }
  try {
    return await new Promise((resolve, reject) => {
      socket.onmessage = (event) => resolve(event.data);
      socket.onclose = (event) => reject(new Error(`closed: ${event.code} ${event.reason}`));
      socket.send("command-check");
    });
  } finally {
    socket.close();
  }
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export class Origin {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  // One socket kept open across ten round trips, well past the end of the event that started this work.
  async converse(url, wait) {
    await delay(wait);
    const results = [];
    try {
      const socket = new WebSocket(url.replace("http:", "ws:"));
      await new Promise((resolve, reject) => {
        socket.onopen = resolve;
        socket.onclose = (event) => reject(new Error(`opening: ${event.code} ${event.reason}`));
      });
      for (let i = 0; i < 10; i++) {
        await delay(100);
        results.push({
          echo: await new Promise((resolve, reject) => {
            socket.onmessage = (event) => resolve(event.data);
            socket.onclose = (event) => reject(new Error(`closed: ${event.code} ${event.reason}`));
            socket.send("ping");
          }),
        });
      }
      socket.close();
    } catch (error) {
      results.push({ error: error.message });
    }
    await this.state.storage.put("results", results);
  }

  async run(url, mode) {
    const results = [];
    for (let i = 0; i < 20; i++) {
      try {
        const echo = await exchange(url, this.env, i === 0 ? "constructor" : mode);
        results.push({ echo });
      } catch (error) {
        results.push({ error: error.message });
      }
      await this.state.storage.put("results", results);
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/start") {
      this.state.waitUntil(this.run(`${url.origin}/echo`, url.searchParams.get("mode")));
      return new Response("started");
    }
    if (url.pathname === "/detached") {
      // `short` ends the event while the socket is open, and `none` ends it before the socket exists.
      const short = url.searchParams.get("settle") === "short";
      if (short) this.state.waitUntil(delay(200));
      void this.converse(`${url.origin}/worker-echo`, short ? 0 : 300);
      return new Response("started");
    }
    return Response.json(await this.state.storage.get("results") ?? []);
  }
}

export class Echo {
  constructor(state) {
    this.state = state;
  }

  fetch() {
    const [client, server] = new WebSocketPair();
    this.state.acceptWebSocket(server);
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(socket, message) {
    socket.send(message);
    socket.close(1000, "done");
  }
}

export default {
  fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/health") return new Response("ok");
    if (url.pathname === "/worker-echo") {
      const [client, server] = new WebSocketPair();
      server.accept();
      server.addEventListener("message", (event) => server.send(event.data));
      return new Response(null, { status: 101, webSocket: client });
    }
    if (url.pathname === "/echo") return env.ECHO.getByName("echo").fetch(request);
    return env.ORIGIN.getByName(url.searchParams.get("id")).fetch(request);
  },
};
