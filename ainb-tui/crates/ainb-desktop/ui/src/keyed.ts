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

/** Separates a repeated key from its count. No id the host sends carries it. */
const REPEAT = String.fromCharCode(0);

/**
 * `items` keyed by `keyOf`, keeping the order they came in.
 *
 * Two items that name the same key both stay: the second and later carry a
 * suffix, so a duplicate cannot drop a row from the list or make one row's
 * node draw another row's text.
 */
export function keyedList<T>(items: readonly T[], keyOf: (item: T) => string): KeyedList<T> {
  const keys: string[] = [];
  const byKey = new Map<string, T>();
  const seen = new Map<string, number>();
  for (const item of items) {
    const base = keyOf(item);
    const count = (seen.get(base) ?? 0) + 1;
    seen.set(base, count);
    const key = count === 1 ? base : `${base}${REPEAT}${count}`;
    keys.push(key);
    byKey.set(key, item);
  }
  return { keys, byKey };
}

/** Whether two key lists name the same rows in the same order. */
export function sameKeys(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((key, index) => key === b[index]);
}
