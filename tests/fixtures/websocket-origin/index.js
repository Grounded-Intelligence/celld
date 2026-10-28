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

export class Origin {
  constructor(state, env) {
    this.state = state;
    this.env = env;
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
