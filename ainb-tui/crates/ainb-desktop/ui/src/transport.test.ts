// The local leg's three calls, and the credit rule behind them: the listener
// is in place before the output channel is opened, and every delivery is
// acknowledged once, however the paint went.

import assert from "node:assert/strict";
import { test } from "node:test";
import { tauriTransport, type Bridge, type ByteChannel } from "./transport.ts";

interface Call {
  command: string;
  args: Record<string, unknown>;
}

/** A bridge that records what was invoked and hands back its channel. */
function bridge() {
  const calls: Call[] = [];
  const channel: ByteChannel = { onmessage: () => assert.fail("no listener was installed") };
  const made: ByteChannel[] = [];
  const spy: Bridge = {
    invoke(command, args) {
      calls.push({ command, args });
      return Promise.resolve(null);
    },
    channel() {
      made.push(channel);
      return channel;
    },
  };
  return { calls, channel, made, spy };
}

/** Let the acknowledgement's promise chain run. */
const settle = (): Promise<void> => new Promise((resume) => setTimeout(resume, 0));

const buffer = (...bytes: number[]) => new Uint8Array(bytes).buffer;

test("typing and resizing reach the host under the tab's key", () => {
  const { calls, spy } = bridge();
  const transport = tauriTransport("tab-1", spy);
  transport.send("ls\r");
  transport.resize(120, 40);
  assert.deepEqual(calls, [
    { command: "terminal_input", args: { key: "tab-1", data: "ls\r" } },
    { command: "terminal_resize", args: { key: "tab-1", cols: 120, rows: 40 } },
  ]);
});

test("the output channel is opened only once its listener is installed", () => {
  const { calls, channel, made, spy } = bridge();
  const painted: number[][] = [];
  tauriTransport("tab-1", spy).onBytes((bytes) => void painted.push([...bytes]));
  assert.equal(made.length, 1);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "terminal_output");
  assert.equal(calls[0].args.bytes, channel, "the host writes to the channel that already has the listener");
  channel.onmessage(buffer(104, 105));
  assert.deepEqual(painted, [[104, 105]]);
});

test("credit comes back once per delivery, with the bytes painted", async () => {
  const { calls, channel, spy } = bridge();
  tauriTransport("tab-1", spy).onBytes(async () => await settle());
  channel.onmessage(buffer(1, 2, 3));
  assert.equal(calls.length, 1, "the acknowledgement waits for the paint");
  await settle();
  await settle();
  assert.deepEqual(calls[1], { command: "terminal_ack", args: { key: "tab-1", bytes: 3 } });
  assert.equal(calls.length, 2);
});

test("a paint that throws or rejects still returns its credit", async () => {
  for (const paint of [
    () => {
      throw new Error("no renderer");
    },
    () => Promise.reject(new Error("write failed")),
  ]) {
    const { calls, channel, spy } = bridge();
    tauriTransport("tab-1", spy).onBytes(paint);
    channel.onmessage(buffer(7));
    await settle();
    assert.deepEqual(calls[1], { command: "terminal_ack", args: { key: "tab-1", bytes: 1 } });
  }
});
