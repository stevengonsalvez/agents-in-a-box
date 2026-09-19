// Keying a drawn list, including the case a list must survive: two rows that
// name the same key, whatever the ids behind them contain.

import assert from "node:assert/strict";
import { test } from "node:test";
import { keyedList, sameKeys } from "./keyed.ts";

const NUL = String.fromCharCode(0);

/** Ids picked to break a scheme that appends a separator to a repeat. */
const ADVERSARIAL = [
  "a",
  "b",
  // Looks like what a count-and-separator scheme produces.
  "1:a",
  "2:a",
  `a${NUL}2`,
  `a${NUL}`,
  `${NUL}a`,
  ":a",
  "::",
  "",
  "10:a",
];

test("a list keeps its order, and every key finds its own item", () => {
  const list = keyedList([{ id: "a" }, { id: "b" }, { id: "c" }], (item) => item.id);
  assert.deepEqual(
    list.keys.map((key) => list.byKey.get(key)?.id),
    ["a", "b", "c"],
  );
  assert.equal(new Set(list.keys).size, 3);
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
});

// The real proof, not a comment: every id above, each repeated three times and
// shuffled together, must still come out as distinct keys that each find the
// row they were made for. A scheme that relied on a character no id carries
// fails here on the ids that carry it.
test("keys stay distinct and correct for ids built to break the scheme", () => {
  const rows = ADVERSARIAL.flatMap((id, index) =>
    [0, 1, 2].map((copy) => ({ id, tag: `${index}-${copy}` })),
  );
  // Interleaved, so repeats of one id are not adjacent.
  rows.sort((a, b) => a.tag.slice(-1).localeCompare(b.tag.slice(-1)));

  const list = keyedList(rows, (row) => row.id);

  assert.equal(list.keys.length, rows.length, "every row is drawn");
  assert.equal(new Set(list.keys).size, rows.length, "no two rows share a key");
  list.keys.forEach((key, index) => {
    assert.equal(list.byKey.get(key)?.tag, rows[index].tag, `key ${JSON.stringify(key)} found the wrong row`);
  });
});

test("one id's rows and another id's rows never collide", () => {
  // `a` twice against the literal id `2:a`: a scheme that spells the second
  // `a` as `2:a` without the count being unambiguous would lose one of them.
  const rows = [{ id: "a" }, { id: "2:a" }, { id: "a" }, { id: "1:a" }];
  const list = keyedList(rows, (row) => row.id);
  assert.equal(new Set(list.keys).size, 4);
  list.keys.forEach((key, index) => assert.equal(list.byKey.get(key)?.id, rows[index].id));
});

test("key lists compare by value, so an equal frame is not a change", () => {
  assert.ok(sameKeys(["a", "b"], ["a", "b"]));
  assert.ok(!sameKeys(["a", "b"], ["b", "a"]), "order counts");
  assert.ok(!sameKeys(["a"], ["a", "b"]), "length counts");
});
