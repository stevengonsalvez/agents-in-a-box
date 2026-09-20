// The inbox journey (D3p-d): issues created through the daemon's own RPC by
// a process that is not the window land in the local human's inbox as the
// daemon aggregates them, the window's inbox page lists them by the daemon's
// own entry ids, the sweep from the page reaches the daemon, and the daemon's
// record is what the page then shows. The ids are asserted by value against
// `ainb hangar inbox list`, so a page drawing rows from anywhere but section
// 16 cannot pass.
//
// The issues go through `hangar/issue_create` rather than the CLI: the CLI
// writes the store directly and stamps its own creator, and the inbox is
// aggregated from the events the daemon's broker carries, for the creator of
// an unassigned issue, so only an issue the daemon created for `member:me`
// is one the local human's inbox holds.

import assert from "node:assert/strict";
import { rpc } from "../rpc.js";
import { click } from "../support.js";
import { AINB_BIN, run } from "../world.js";

/** The local human's inbox as the daemon's own store lists it, newest first. */
function inboxList() {
  return JSON.parse(run(AINB_BIN, ["hangar", "inbox", "list", "--format", "json", "--limit", "200"]));
}

/** Poll `read` until `ok(value)`, or fail with `what` and the last value. */
async function until(read, ok, what, timeout = 60_000) {
  let last;
  const deadline = Date.now() + timeout;
  for (;;) {
    last = read();
    if (ok(last)) return last;
    assert.ok(Date.now() < deadline, `${what}; last: ${JSON.stringify(last).slice(0, 600)}`);
    await browser.pause(1000);
  }
}

describe("the inbox from the window", () => {
  it("lists the daemon's entries by id, sweeps them read, and shows the daemon's record", async () => {
    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: async () => `the sidecar never connected: ${await $(".banner").getText()}`,
    });

    // Two issues, created through the daemon by a process that is not the
    // window, for the local human and unassigned, so they land in the
    // creator's own inbox: `member:me`'s.
    const stamp = Date.now();
    const issues = [];
    for (const n of ["one", "two"]) {
      const title = `e2e inbox ${stamp} ${n}`;
      const reply = await rpc("hangar/issue_create", { workspace_id: "default", title, creator: "member:me" });
      assert.ok(reply.result?.id, `the daemon refused the issue: ${JSON.stringify(reply.error ?? reply)}`);
      issues.push({ id: reply.result.id, title });
    }
    const mine = (entries) => entries.filter((entry) => issues.some((issue) => issue.id === entry.subject_id));

    // The daemon's own record first: the entries exist, unread, with ids of
    // the daemon's minting, before the window is asked for anything.
    const seeded = mine(
      await until(inboxList, (entries) => mine(entries).length === issues.length, "the daemon never aggregated both issues into the inbox"),
    );
    assert.ok(seeded.every((entry) => entry.read_at === null), "the seeded entries start unread");
    const ids = seeded.map((entry) => entry.id);

    // The page lists those entries, by the daemon's ids, unread, with the
    // issue's title in the daemon's summary.
    await click(".inbox-button");
    await $(".inbox").waitForExist({ timeout: 60_000 });
    for (const id of ids) {
      await $(`.inbox-row[data-entry="${id}"]`).waitForExist({
        timeout: 60_000,
        timeoutMsg: `the page never drew entry ${id}`,
      });
    }
    for (const entry of seeded) {
      const row = await $(`.inbox-row[data-entry="${entry.id}"]`);
      const issue = issues.find((candidate) => candidate.id === entry.subject_id);
      assert.ok((await row.getText()).includes(issue.title), `row ${entry.id} carries the issue's title`);
      assert.ok((await row.getAttribute("class")).split(/\s+/).includes("unread"), `row ${entry.id} is drawn unread`);
    }
    const drawn = await Promise.all((await $$(".inbox-row")).map((row) => row.getAttribute("data-entry")));
    assert.deepEqual(
      ids.filter((id) => !drawn.includes(id)),
      [],
      "every entry the daemon lists for the local human is on the page",
    );

    // The sweep from the page: the daemon records every entry read, and the
    // page shows that record, not a local flip.
    await click(".inbox-sweep");
    const swept = mine(
      await until(inboxList, (entries) => mine(entries).every((entry) => entry.read_at !== null), "the daemon never recorded the sweep"),
    );
    assert.equal(swept.length, issues.length);
    await browser.waitUntil(
      async () => {
        for (const id of ids) {
          const cls = await $(`.inbox-row[data-entry="${id}"]`).getAttribute("class");
          if (cls.split(/\s+/).includes("unread")) return false;
        }
        return true;
      },
      { timeout: 60_000, timeoutMsg: "the page kept drawing the swept rows unread" },
    );
    assert.ok(!(await $(".inbox-unread").isExisting()), "the inbox button carries no unread badge after the sweep");

    await click(".inbox-close");
    await $(".inbox").waitForExist({ timeout: 30_000, reverse: true });
  });
});
