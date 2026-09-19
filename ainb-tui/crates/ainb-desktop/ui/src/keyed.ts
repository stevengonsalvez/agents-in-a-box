// Drawing a list by key rather than by object identity (#1267).
//
// Every frame the host sends rebuilds the rows a view projects from it, and
// Solid's `For` keys by identity, so a list of freshly built objects re-creates
// every node several times a second: a click can land on a node that has just
// been replaced. Keys are strings, equal from one frame to the next, so the
// list patches instead, and a row a pointer found is still the row on screen.

/** A list drawn by key: `keys` in the order they draw, `byKey` to read one. */
export interface KeyedList<T> {
  keys: string[];
  byKey: Map<string, T>;
}

/**
 * The key of the `count`th item (1-based) whose own key is `base`.
 *
 * `<count>:<base>`, which is one-to-one for every possible `base`: the digits
 * before the first colon are the count and everything after it is the base, so
 * two different (count, base) pairs cannot spell one key, whatever the host's
 * ids carry, colons and control characters included.
 *
 * An earlier version suffixed a repeat with a NUL and relied on no id carrying
 * one. Nothing enforced that, so a single id with a NUL in it would have
 * silently made two rows share a key.
 */
function keyFor(count: number, base: string): string {
  return `${count}:${base}`;
}

/**
 * `items` keyed by `keyOf`, keeping the order they came in.
 *
 * Two items that name the same key both stay, each with a key of its own, so a
 * duplicate cannot drop a row from the list or make one row's node draw
 * another row's text.
 */
export function keyedList<T>(items: readonly T[], keyOf: (item: T) => string): KeyedList<T> {
  const keys: string[] = [];
  const byKey = new Map<string, T>();
  const seen = new Map<string, number>();
  for (const item of items) {
    const base = keyOf(item);
    const count = (seen.get(base) ?? 0) + 1;
    seen.set(base, count);
    const key = keyFor(count, base);
    keys.push(key);
    byKey.set(key, item);
  }
  return { keys, byKey };
}

/** Whether two key lists name the same rows in the same order. */
export function sameKeys(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((key, index) => key === b[index]);
}
