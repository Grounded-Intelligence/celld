// node:net for celld -- the address predicates, ported from Node.js's
// lib/internal/net.js. Copyright Node.js contributors (MIT).
//
// A Worker opens sockets through cloudflare:sockets, so the socket and server
// exports stay unsupported and throw at first use. Clients such as pg call
// isIP() before a TLS upgrade to decide whether to send SNI.
//
// Injected lazily, so an isolate that never imports node:net pays nothing.
(() => {
  "use strict";

  const v4Seg = "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])";
  const v4Str = `(?:${v4Seg}\\.){3}${v4Seg}`;
  const IPv4Reg = new RegExp(`^${v4Str}$`);

  const v6Seg = "(?:[0-9a-fA-F]{1,4})";
  const IPv6Reg = new RegExp(
    "^(?:" +
      `(?:${v6Seg}:){7}(?:${v6Seg}|:)|` +
      `(?:${v6Seg}:){6}(?:${v4Str}|:${v6Seg}|:)|` +
      `(?:${v6Seg}:){5}(?::${v4Str}|(?::${v6Seg}){1,2}|:)|` +
      `(?:${v6Seg}:){4}(?:(?::${v6Seg}){0,1}:${v4Str}|(?::${v6Seg}){1,3}|:)|` +
      `(?:${v6Seg}:){3}(?:(?::${v6Seg}){0,2}:${v4Str}|(?::${v6Seg}){1,4}|:)|` +
      `(?:${v6Seg}:){2}(?:(?::${v6Seg}){0,3}:${v4Str}|(?::${v6Seg}){1,5}|:)|` +
      `(?:${v6Seg}:){1}(?:(?::${v6Seg}){0,4}:${v4Str}|(?::${v6Seg}){1,6}|:)|` +
      `(?::(?:(?::${v6Seg}){0,5}:${v4Str}|(?::${v6Seg}){1,7}|:))` +
      ")(?:%[0-9a-zA-Z-.:]{1,})?$",
  );

  const isIPv4 = (input) => IPv4Reg.test(`${input}`);
  const isIPv6 = (input) => IPv6Reg.test(`${input}`);
  const isIP = (input) => {
    if (isIPv4(input)) return 4;
    if (isIPv6(input)) return 6;
    return 0;
  };

  __celld.__netModule = __celld.__partialNamespace(
    { isIP, isIPv4, isIPv6 },
    "node:net",
    [
      "BlockList", "Server", "Socket", "SocketAddress", "Stream",
      "_createServerHandle", "_normalizeArgs", "connect", "createConnection",
      "createServer", "getDefaultAutoSelectFamily",
      "getDefaultAutoSelectFamilyAttemptTimeout", "setDefaultAutoSelectFamily",
      "setDefaultAutoSelectFamilyAttemptTimeout",
    ],
  );
})();
