// Which of the reducer's notices are new since the last Shell frame.
//
// The notices carry no id, and the reducer caps the list and drains it from
// the front, so a count cannot say what is new: once the list is full a new
// notice leaves the length unchanged, and after a drain old ones would read as
// new. Each notice is keyed by its type and text instead, counted, so the same
// text raised twice is two notices.

import type { Notification_Serialize } from "../../../ainb-app/bindings/AppState";

/** A notice's identity: its type and its text. */
export function noticeKey(notice: Notification_Serialize): string {
  return `${notice.notification_type}\n${notice.message}`;
}

/** The notices in `current` that `previous` (the last frame's keys) did not hold. */
export function newNotices(
  previous: readonly string[],
  current: readonly Notification_Serialize[],
): Notification_Serialize[] {
  const seen = new Map<string, number>();
  for (const key of previous) seen.set(key, (seen.get(key) ?? 0) + 1);
  const fresh: Notification_Serialize[] = [];
  for (const notice of current) {
    const key = noticeKey(notice);
    const left = seen.get(key) ?? 0;
    if (left > 0) seen.set(key, left - 1);
    else fresh.push(notice);
  }
  return fresh;
}
