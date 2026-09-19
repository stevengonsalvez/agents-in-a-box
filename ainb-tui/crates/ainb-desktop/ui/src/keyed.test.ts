// Keying a drawn list, including the case a list must survive: two rows that
// name the same key.

import assert from "node:assert/strict";
import { test } from "node:test";
import { keyedList, sameKeys } from "./keyed.ts";

test("a list keeps its order, and every key finds its own item", () => {
  const list = keyedList([{ id: "a" }, { id: "b" }, { id: "c" }], (item) => item.id);
  assert.deepEqual(list.keys, ["a", "b", "c"]);
  assert.deepEqual(
    list.keys.map((key) => list.byKey.get(key)?.id),
    ["a", "b", "c"],
  );
});

test("two rows naming one key both stay, each finding its own item", () => {
  const rows = [
    { id: "dup", title: "first" },
    { id: "other", title: "other" },
    { id: "dup", title: "second" },
  ];
  const list = keyedList(rows, (row) => row.id);

  assert.equal(list.keys.length, 3, "no row is dropped");
  assert.equal(new Set(list.keys).size, 3, "no two rows share a key");
  assert.deepEqual(
    list.keys.map((key) => list.byKey.get(key)?.title),
    ["first", "other", "second"],
    "each key finds the row it was made for",
  );
  assert.equal(list.keys[0], "dup", "the first of a repeat keeps the plain key");
});

test("a suffixed key cannot collide with a key the host sends", () => {
  // The separator is NUL, so a host id that looks like a suffix is still its
  // own row: nothing is overwritten.
  const list = keyedList([{ id: "dup" }, { id: "dup#2" }, { id: "dup" }], (item) => item.id);
  assert.equal(new Set(list.keys).size, 3);
  assert.ok(list.keys.includes("dup#2"), "the host's own id is untouched");
});

test("key lists compare by value, so an equal frame is not a change", () => {
  assert.ok(sameKeys(["a", "b"], ["a", "b"]));
  assert.ok(!sameKeys(["a", "b"], ["b", "a"]), "order counts");
  assert.ok(!sameKeys(["a"], ["a", "b"]), "length counts");
});
