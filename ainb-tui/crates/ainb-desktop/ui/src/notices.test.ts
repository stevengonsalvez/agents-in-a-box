// New notices by identity, not by count.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { Notification_Serialize } from "../../../ainb-app/bindings/AppState";
import { newNotices, noticeKey } from "./notices.ts";

const notice = (message: string, notification_type = "Info"): Notification_Serialize =>
  ({ message, notification_type, duration: { secs: 5, nanos: 0 } }) as Notification_Serialize;

test("a full list that drops its oldest still yields the new notice", () => {
  const before = ["a", "b", "c"].map((m) => notice(m));
  const after = ["b", "c", "d"].map((m) => notice(m));
  assert.deepEqual(
    newNotices(before.map(noticeKey), after).map((n) => n.message),
    ["d"],
  );
});

test("a drain re-toasts nothing, and the same text raised again is new", () => {
  const before = ["a", "b"].map((m) => notice(m));
  assert.deepEqual(newNotices(before.map(noticeKey), [notice("b")]), []);
  assert.deepEqual(
    newNotices([noticeKey(notice("a"))], [notice("a"), notice("a")]).map((n) => n.message),
    ["a"],
  );
});

test("the type is part of the identity", () => {
  assert.equal(newNotices([noticeKey(notice("x", "Info"))], [notice("x", "Error")]).length, 1);
});
