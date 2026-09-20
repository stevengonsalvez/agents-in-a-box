// One JSON-RPC call to the world's daemon, over its socket, as a surface
// that is not the window makes it: `auth/hello` with the token the daemon
// wrote, then the method. Content-Length framed, as the daemon speaks.

import { readFileSync } from "node:fs";
import { connect } from "node:net";
import { join } from "node:path";
import { env } from "./world.js";

/** `method` with `params`, as the daemon answered it (`result` or `error`). */
export function rpc(method, params = {}) {
  const home = env().AINB_HANGAR_HOME;
  const token = readFileSync(join(home, "hangar", "daemon.token"), "utf8").trim();
  return new Promise((resolve, reject) => {
    const socket = connect(join(home, "hangar.sock"));
    let buffer = Buffer.alloc(0);
    const send = (id, name, body) => {
      const raw = Buffer.from(JSON.stringify({ jsonrpc: "2.0", id, method: name, params: body }));
      socket.write(Buffer.concat([Buffer.from(`Content-Length: ${raw.length}\r\n\r\n`), raw]));
    };
    const frames = () => {
      const out = [];
      for (;;) {
        const head = buffer.indexOf("\r\n\r\n");
        if (head < 0) return out;
        const length = Number(/content-length:\s*(\d+)/i.exec(buffer.subarray(0, head).toString())?.[1]);
        if (buffer.length < head + 4 + length) return out;
        out.push(JSON.parse(buffer.subarray(head + 4, head + 4 + length).toString()));
        buffer = buffer.subarray(head + 4 + length);
      }
    };
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`${method}: the daemon did not answer within 30 s`));
    }, 30_000);
    socket.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    socket.on("connect", () => send(1, "auth/hello", { token, protocol: { min: 1, max: 1 }, capabilities: [] }));
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      for (const frame of frames()) {
        if (frame.id === 1) {
          if (frame.error) {
            clearTimeout(timer);
            socket.end();
            reject(new Error(`auth/hello refused: ${JSON.stringify(frame.error)}`));
            return;
          }
          send(2, method, params);
        } else if (frame.id === 2) {
          clearTimeout(timer);
          socket.end();
          resolve(frame);
        }
      }
    });
  });
}
