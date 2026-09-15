// The renderer's frame store: every section frame the window holds, keyed by
// (host id, section), under that host's boot epoch.
//
//   Channel ──FrameBatch──▶ drain queue ──one batch()──▶ store ──▶ effects, memos
//
// The four D15 invariants live here and nowhere else, as
// `ainb_app::wire::store::MirrorStore` specifies them:
//   1. A frame is held under the host at the other end of the channel it came
//      over. Two hosts' sections never merge, and a frame's own `host_id`
//      never picks the key.
//   2. A larger epoch from a host drops everything held from that host first;
//      a smaller one is a frame from a dead process and applies nothing.
//   3. One drain is one `batch()`: readers see the whole drain or none of it,
//      and effects run once, after the commit.
//   4. A section the window did not subscribe to applies nothing.
// Root selectors return scalars, so a drain that leaves a count unchanged wakes
// nothing that reads the count.

import { batch, createMemo, type Accessor } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type {
  FrameBatch_Serialize,
  Frame_Serialize,
  HostId,
  SectionBodies_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** A section's wire name, as `ainb_app::wire::section_name` spells it. */
export type SectionName = keyof SectionBodies_Serialize;

export interface HeldSection<S extends SectionName> {
  version: number;
  body: SectionBodies_Serialize[S];
}

export type HeldSections = { [S in SectionName]?: HeldSection<S> };

export interface HostFrames {
  epoch: number;
  sections: HeldSections;
}

export interface FrameState {
  hosts: Record<HostId, HostFrames>;
  /** Sections the host withheld as oversize and has not framed since. */
  stale: SectionName[];
}

export interface FrameStore {
  readonly state: FrameState;
  /**
   * Apply every batch received from `peer` since the last drain, as one
   * transaction. `peer` is the host the channel is connected to; a frame
   * naming any other host applies nothing.
   */
  applyDrain(peer: HostId, batches: readonly FrameBatch_Serialize[]): void;
  /** One host's section body, or `undefined` when none is held. */
  section<S extends SectionName>(host: HostId, name: S): SectionBodies_Serialize[S] | undefined;
  /** How many hosts the store holds any section from. */
  hostCount: Accessor<number>;
}

interface Plan {
  epoch: number;
  /** The drain saw a larger epoch: the host's held sections go first. */
  reset: boolean;
  frames: Map<SectionName, Frame_Serialize>;
}

export function createFrameStore(subscribed: readonly SectionName[]): FrameStore {
  const wanted = new Set<string>(subscribed);
  const [state, setState] = createStore<FrameState>({ hosts: {}, stale: [] });

  function applyDrain(peer: HostId, batches: readonly FrameBatch_Serialize[]) {
    // Decide in plain objects first, so the store is written once per
    // (host, section) however many frames the drain carried for it.
    const plans = new Map<HostId, Plan>();
    const withheld = new Set<SectionName>();
    for (const { frames, oversize } of batches) {
      for (const frame of frames) {
        if (!wanted.has(frame.section) || frame.host_id !== peer) continue;
        const name = frame.section as SectionName;
        const held = state.hosts[frame.host_id];
        let plan = plans.get(frame.host_id);
        const epoch = plan?.epoch ?? held?.epoch;
        if (epoch !== undefined && frame.epoch < epoch) continue;
        if (epoch === undefined || frame.epoch > epoch) {
          plan = { epoch: frame.epoch, reset: true, frames: new Map() };
          plans.set(frame.host_id, plan);
        } else if (!plan) {
          plan = { epoch, reset: false, frames: new Map() };
          plans.set(frame.host_id, plan);
        }
        const prior =
          plan.frames.get(name)?.version ?? (plan.reset ? undefined : held?.sections[name]?.version);
        if (prior !== undefined && frame.version <= prior) continue;
        plan.frames.set(name, frame);
        withheld.delete(name);
      }
      for (const section of oversize ?? []) {
        if (wanted.has(section.section)) withheld.add(section.section as SectionName);
      }
    }

    batch(() => {
      for (const [host, plan] of plans) {
        if (plan.reset) setState("hosts", host, { epoch: plan.epoch, sections: {} });
        for (const [name, frame] of plan.frames) {
          const held = state.hosts[host].sections[name];
          if (held) {
            setState("hosts", host, "sections", name, "version", frame.version);
            // Diff against the held body so only changed fields notify.
            setState("hosts", host, "sections", name, "body", reconcile(frame.body as never, { key: "id" }));
          } else {
            setState("hosts", host, "sections", name, {
              version: frame.version,
              body: structuredClone(frame.body),
            } as never);
          }
        }
      }
      const framed = new Set([...plans.values()].flatMap((plan) => [...plan.frames.keys()]));
      const stale = [...new Set([...state.stale.filter((name) => !framed.has(name)), ...withheld])];
      if (stale.length !== state.stale.length || stale.some((name, i) => name !== state.stale[i])) {
        setState("stale", stale);
      }
    });
  }

  function section<S extends SectionName>(host: HostId, name: S) {
    return (state.hosts[host]?.sections[name] as HeldSection<S> | undefined)?.body;
  }

  const hostCount = createMemo(() => Object.keys(state.hosts).length);

  return { state, applyDrain, section, hostCount };
}
