// Must be the first import: stamps JS start before any other module evaluates.
(globalThis as { __jsStartMs?: number }).__jsStartMs = Date.now();
export {};
