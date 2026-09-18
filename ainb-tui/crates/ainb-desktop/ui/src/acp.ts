// What the ACP card draws from the Fleet frame's transcript, and the intents
// that open and close it. Projections only: `acp.tsx` draws what these return.
//
// An ACP session has no tmux pane, so no terminal tab can show it and no
// session list row names it: its board card is the only way in. The card
// stands where the terminal would.

import type {
  ChunkKind,
  FleetView_Serialize,
  TranscriptStatus_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

/**
 * What each chunk kind is called on the card. Typed over every `ChunkKind`, so
 * a kind Rust adds fails to compile here instead of drawing unlabelled.
 */
export const CHUNK_LABELS: Record<ChunkKind, string> = {
  Message: "agent",
  UserMessage: "you",
  Thought: "thinking",
  ToolCall: "tool",
  Plan: "plan",
  Permission: "permission",
  Usage: "usage",
  Lifecycle: "run",
};

/** One chunk as the card draws it. */
export interface ChunkView {
  /** The daemon's ingest order, which keys the rows. */
  key: number;
  kind: ChunkKind;
  label: string;
  /**
   * What the chunk says, through the same rule as every other frame text. A
   * body the host scrubbed whole still reads `<redacted>`: the card draws the
   * marker, never an empty row that looks as if the agent said nothing.
   */
  body: string;
  truncated: boolean;
}

/** The card's picture of the open transcript. */
export interface TranscriptView {
  sessionKey: string;
  /** One line on where the read stands, or `null` while it is live. */
  status: string | null;
  chunks: ChunkView[];
  /** Whether older chunks exist than the card holds. */
  startsPartWay: boolean;
}

function statusLine(status: TranscriptStatus_Serialize): string | null {
  if (typeof status === "object") return `The daemon could not read the transcript: ${label(status.Unavailable.detail)}`;
  switch (status) {
    case "Loading":
      return "Reading the transcript";
    case "Live":
      return null;
    // A closed transcript carries no key, so `transcriptView` never reads one.
    // Named rather than defaulted: a status Rust adds fails this switch.
    case "Closed":
      return null;
  }
}

/**
 * The open transcript as the card draws it, or `null` when the frame holds
 * none for `sessionKey`: a frame still carrying another run, or none yet.
 */
export function transcriptView(fleet: FleetView_Serialize | undefined, sessionKey: string): TranscriptView | null {
  const transcript = fleet?.transcript;
  if (transcript === undefined || transcript.session_key !== sessionKey) return null;
  return {
    sessionKey,
    status: statusLine(transcript.status),
    chunks: transcript.chunks.map((chunk) => ({
      key: chunk.order,
      kind: chunk.kind,
      label: CHUNK_LABELS[chunk.kind],
      // `label` would cut at a sidebar's width; a chunk keeps its line breaks
      // and only loses what cannot be seen.
      body: chunk.body.replace(/[\p{Cf}]/gu, "").replace(/[^\P{Cc}\n\t]/gu, ""),
      truncated: chunk.truncated,
    })),
    startsPartWay: transcript.starts_part_way,
  };
}

/** Open `sessionKey`'s transcript, or close the open one with `null`. */
export function transcriptIntent(sessionKey: string | null): RendererIntent {
  return { Command: ["session_list.open_transcript", { session_key: sessionKey }] };
}
