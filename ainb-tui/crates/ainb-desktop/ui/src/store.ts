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

import { batch, createMemo, createSignal, type Accessor } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type {
  DaemonRead,
  FrameBatch_Serialize,
  Frame_Serialize,
  HostId,
  SectionBodies_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** A section's wire name, as `ainb_app::wire::section_name` spells it. */
export type SectionName = keyof SectionBodies_Serialize;

export interface HeldSection<S extends SectionName> {
  version: number;
  /**
   * The daemon read behind the body, for a section that has one: the D18
   * fence value and the revision a stale badge names.
   */
  daemon_read: DaemonRead | null;
  body: SectionBodies_Serialize[S];
}

export type HeldSections = { [S in SectionName]?: HeldSection<S> };

export interface HostFrames {
  epoch: number;
  sections: HeldSections;
}

/** Sections a host withheld as oversize and has not framed since. */
export type StaleSections = { [S in SectionName]?: true };

export interface FrameState {
  hosts: Record<HostId, HostFrames>;
  /** Keyed like `hosts`: one host's oversize notice never marks another's copy. */
  stale: Record<HostId, StaleSections>;
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
  /**
   * Frames that applied nothing since the store was made, as
   * `wire::store::MirrorStore::frames_ignored` counts them: an unsubscribed
   * section, a host other than the channel's peer, a host past `MAX_HOSTS`, an
   * older epoch, or a version at or below the one held (#1132). A frame a
   * later one of the same section replaced within a drain is not counted.
   */
  framesIgnored: Accessor<number>;
  /**
   * Drop everything held from `host`, its stale marks included, as
   * `wire::store::MirrorStore::evict_host`: the host went away, or the window's
   * peer was re-pinned to a new id (#1066). The slot no longer counts toward
   * `MAX_HOSTS`, and a later frame from the host starts it over.
   */
  evictHost(host: HostId): void;
}

/**
 * The most distinct hosts one store holds, as `wire::store::MAX_HOSTS`. A drain
 * from a host beyond it applies nothing, so a misbehaving peer set cannot grow
 * the store without bound. `evictHost` makes room.
 */
export const MAX_HOSTS = 64;

interface Plan {
  epoch: number;
  /** The drain saw a larger epoch: the host's held sections go first. */
  reset: boolean;
  frames: Map<SectionName, Frame_Serialize>;
}

export function createFrameStore(subscribed: readonly SectionName[]): FrameStore {
  const wanted = new Set<string>(subscribed);
  const [state, setState] = createStore<FrameState>({ hosts: {}, stale: {} });
  const [framesIgnored, setFramesIgnored] = createSignal(0);

  function applyDrain(peer: HostId, batches: readonly FrameBatch_Serialize[]) {
    const hosts = new Set([...Object.keys(state.hosts), ...Object.keys(state.stale)]);
    if (!hosts.has(peer) && hosts.size >= MAX_HOSTS) {
      const refused = batches.reduce((sum, { frames }) => sum + frames.length, 0);
      if (refused > 0) batch(() => setFramesIgnored((count) => count + refused));
      return;
    }
    let ignored = 0;
    // Decide in plain objects first, so the store is written once per
    // (host, section) however many frames the drain carried for it.
    const plans = new Map<HostId, Plan>();
    const withheld = new Set<SectionName>();
    for (const { frames, oversize } of batches) {
      for (const frame of frames) {
        if (!wanted.has(frame.section) || frame.host_id !== peer) {
          ignored += 1;
          continue;
        }
        const name = frame.section as SectionName;
        const held = state.hosts[frame.host_id];
        let plan = plans.get(frame.host_id);
        const epoch = plan?.epoch ?? held?.epoch;
        if (epoch !== undefined && frame.epoch < epoch) {
          ignored += 1;
          continue;
        }
        if (epoch === undefined || frame.epoch > epoch) {
          plan = { epoch: frame.epoch, reset: true, frames: new Map() };
          plans.set(frame.host_id, plan);
        } else if (!plan) {
          plan = { epoch, reset: false, frames: new Map() };
          plans.set(frame.host_id, plan);
        }
        const prior =
          plan.frames.get(name)?.version ?? (plan.reset ? undefined : held?.sections[name]?.version);
        if (prior !== undefined && frame.version <= prior) {
          ignored += 1;
          continue;
        }
        plan.frames.set(name, frame);
        withheld.delete(name);
      }
      // An oversize notice names no host: it came over this channel, so it is
      // the peer's.
      for (const section of oversize ?? []) {
        if (wanted.has(section.section)) withheld.add(section.section as SectionName);
      }
    }

    batch(() => {
      if (ignored > 0) setFramesIgnored((count) => count + ignored);
      for (const [host, plan] of plans) {
        if (plan.reset) setState("hosts", host, { epoch: plan.epoch, sections: {} });
        for (const [name, frame] of plan.frames) {
          const held = state.hosts[host].sections[name];
          if (held) {
            setState("hosts", host, "sections", name, "version", frame.version);
            setState("hosts", host, "sections", name, "daemon_read", frame.daemon_read ?? null);
            // Diff against the held body so only changed fields notify.
            setState("hosts", host, "sections", name, "body", reconcile(frame.body as never, { key: "id" }));
          } else {
            setState("hosts", host, "sections", name, {
              version: frame.version,
              daemon_read: frame.daemon_read ?? null,
              body: structuredClone(frame.body),
            } as never);
          }
        }
      }
      // Only the peer's own frames clear the peer's stale entries; a restart
      // clears them all, since the new process frames every section again.
      const plan = plans.get(peer);
      if (plan?.reset && state.stale[peer]) setState("stale", { [peer]: {} });
      if (withheld.size > 0 && !state.stale[peer]) setState("stale", { [peer]: {} });
      for (const name of plan?.frames.keys() ?? []) {
        if (state.stale[peer]?.[name]) setState("stale", peer, name, undefined);
      }
      for (const name of withheld) {
        if (!state.stale[peer]?.[name]) setState("stale", peer, name, true);
      }
    });
  }

  function section<S extends SectionName>(host: HostId, name: S) {
    return (state.hosts[host]?.sections[name] as HeldSection<S> | undefined)?.body;
  }

  const hostCount = createMemo(() => Object.keys(state.hosts).length);

  function evictHost(host: HostId) {
    batch(() => {
      // Setting a key to `undefined` deletes it from a Solid store, so the
      // host leaves the key sets `applyDrain` counts against `MAX_HOSTS`.
      setState("hosts", host, undefined as never);
      setState("stale", host, undefined as never);
    });
  }

  return { state, applyDrain, section, hostCount, framesIgnored, evictHost };
}
