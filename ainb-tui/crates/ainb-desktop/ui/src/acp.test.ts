// The ACP card's picture of a transcript, and the intents that open and close it.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AgentCardFrame,
  AgentStatusView,
  ChunkKind,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { CHUNK_LABELS, transcriptIntent, transcriptView } from "./acp.ts";
import { boardColumns } from "./board.ts";

const KINDS: ChunkKind[] = ["Message", "UserMessage", "Thought", "ToolCall", "Plan", "Permission", "Usage", "Lifecycle"];

// Built from code points, so no invisible character sits in this file.
const RIGHT_TO_LEFT_OVERRIDE = String.fromCodePoint(0x202e);
const BELL = String.fromCodePoint(0x07);

function fleet(body: (kind: ChunkKind) => string, status: unknown = "Live"): FleetView_Serialize {
  return {
    transcript: {
      session_key: "acp:s-1",
      status,
      chunks: KINDS.map((kind, order) => ({ order, kind, body: body(kind), truncated: false })),
      chunks_held: KINDS.length,
      starts_part_way: false,
    },
  } as unknown as FleetView_Serialize;
}

test("each chunk kind draws, with its own label", () => {
  const view = transcriptView(
    fleet((kind) => `a ${kind} chunk`),
    "acp:s-1",
  )!;
  assert.deepEqual(
    view.chunks.map((chunk) => [chunk.kind, chunk.label, chunk.body]),
    KINDS.map((kind) => [kind, CHUNK_LABELS[kind], `a ${kind} chunk`]),
  );
  assert.equal(view.status, null, "a live read says nothing");
});

test("a scrubbed chunk draws its redaction marker, not an empty row", () => {
  const view = transcriptView(
    fleet(() => "<redacted>"),
    "acp:s-1",
  )!;
  assert.equal(view.chunks.length, KINDS.length, "every scrubbed chunk is still drawn");
  assert.ok(view.chunks.every((chunk) => chunk.body === "<redacted>"));
});

test("a chunk keeps its lines and loses what cannot be seen", () => {
  const view = transcriptView(
    fleet(() => `first\nsec${RIGHT_TO_LEFT_OVERRIDE}ond${BELL}`),
    "acp:s-1",
  )!;
  assert.equal(view.chunks[0].body, "first\nsecond");
});

test("a frame carrying another run, or none, draws no transcript for this card", () => {
  assert.equal(
    transcriptView(
      fleet(() => "x"),
      "acp:other",
    ),
    null,
  );
  assert.equal(transcriptView(undefined, "acp:s-1"), null);
});

test("an unreachable read says why, in the daemon client's words", () => {
  const view = transcriptView(
    fleet(() => "x", { Unavailable: { detail: "socket gone" } }),
    "acp:s-1",
  )!;
  assert.match(view.status!, /socket gone/);
});

test("a session with no tmux is a card that opens its transcript where the tab would be", () => {
  // No session list row names an ACP session: its board card is the way in,
  // and it is offered rather than drawn disabled.
  const status: AgentStatusView = {
    absent: null,
    head_revision: 1,
    view: {
      host_id: "local",
      read_revision: 1,
      received_at_ms: 1,
      head_revision: 1,
      health: { kind: "live" },
      cards: [
        {
          session_key: "acp:s-1",
          provider: "acp",
          lifecycle: "RUNNING",
          transport_health: "HEALTHY",
          state: "working",
          has_open_request: false,
          wait_kind: null,
        } as AgentCardFrame,
      ],
    },
  };
  const sessions = { workspaces: [] } as unknown as SessionsView_Serialize;
  const card = boardColumns(status, {} as FleetView_Serialize, sessions)
    .flatMap((column) => column.cards)
    .find((c) => c.key === "acp:s-1")!;
  assert.equal(card.sessionId, null, "no session list row");
  assert.equal(card.acp, true, "so the card opens the transcript");
  assert.deepEqual(transcriptIntent(card.key), {
    Command: ["session_list.open_transcript", { session_key: "acp:s-1" }],
  });
  assert.deepEqual(transcriptIntent(null), {
    Command: ["session_list.open_transcript", { session_key: null }],
  });
});
