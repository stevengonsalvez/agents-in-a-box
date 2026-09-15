// A terminal tab's byte stream, whatever carries it. The local leg is the
// window's own channel to the Rust-owned PTY (`terminal_output` and friends);
// a remote leg (R1) implements the same three calls over the box's WS.

import { Channel, invoke } from "@tauri-apps/api/core";

export interface TerminalTransport {
  /** Typed or pasted text for the pane. */
  send(data: string): void;
  /** The grid the tab now shows. */
  resize(cols: number, rows: number): void;
  /**
   * Take the pane's output. The listener resolves once the bytes are painted:
   * only then is more sent, so a slow paint holds output back at its source.
   */
  onBytes(listener: (bytes: Uint8Array) => Promise<void> | void): void;
}

/** The local leg: tab `key`'s output over a raw-buffer channel. */
export function tauriTransport(key: string): TerminalTransport {
  return {
    send: (data) => void invoke("terminal_input", { key, data }),
    resize: (cols, rows) => void invoke("terminal_resize", { key, cols, rows }),
    onBytes(listener) {
      // The channel opens only now, with its listener in place, so no output
      // arrives before there is somewhere to paint it and nothing is buffered.
      const output = new Channel<ArrayBuffer>();
      output.onmessage = (buffer) => {
        const bytes = new Uint8Array(buffer);
        const acknowledge = () => {
          invoke("terminal_ack", { key, bytes: bytes.byteLength }).catch((error: unknown) =>
            console.warn("terminal acknowledgement not delivered", error),
          );
        };
        // Credit comes back however the paint went: a throwing or rejected
        // paint must not leave the tab's window closed.
        try {
          void Promise.resolve(listener(bytes))
            .catch((error: unknown) => console.warn("terminal paint failed", error))
            .finally(acknowledge);
        } catch (error) {
          console.warn("terminal paint failed", error);
          acknowledge();
        }
      };
      void invoke<boolean>("terminal_output", { key, bytes: output });
    },
  };
}
