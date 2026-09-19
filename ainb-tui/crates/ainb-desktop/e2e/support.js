// Helpers every desktop journey spec shares, so a spec does not grow its own
// copy of how to click, how to type into the palette, or how to read back what
// the host did with what the window sent.

import { env, run } from "./world.js";

/**
 * Click `selector`, re-finding it each try until it lands.
 *
 * Every list in this window is redrawn on every frame the host sends, and a
 * frame arrives whenever anything moves: an element found a moment ago can be
 * detached before the click reaches it, which WebDriver reports as a stale
 * reference. That is the window working, not failing, so the click waits it
 * out and only the absence of the element is a failure. The click goes
 * through the driver, so it passes WebDriver's actionability checks
 * (displayed, enabled, not covered), which a click the page runs on itself
 * never proves.
 *
 * Lifted from lane F's review spec (#1257) so every spec shares one.
 */
// How a driver says the element went away between finding it and clicking it.
// Each driver has its own words for it: WebKitGTK reports a click on a
// detached element as "element wasn't found", which reads like a missing
// selector but means the same as a stale reference.
const DETACHED = /stale element|no longer attached|not interactable|element wasn't found|element not found/i;

export async function click(selector, timeout = 60_000) {
  let last = null;
  try {
    await browser.waitUntil(
      async () => {
        try {
          const element = await $(selector);
          if (!(await element.isExisting())) return false;
          await element.click();
          return true;
        } catch (error) {
          last = error;
          if (!DETACHED.test(String(error))) throw error;
          return false;
        }
      },
      { timeout },
    );
  } catch (error) {
    // `waitUntil` takes its message as a string fixed at the call, so the
    // last reason the click failed is only known here.
    if (!/timed out/i.test(String(error))) throw error;
    const why = last === null ? "the element never existed" : String(last);
    throw new Error(`${selector} never took a click within ${timeout} ms: ${why}`);
  }
}

/**
 * Put `text` in the palette's query, replacing what is there.
 *
 * The palette keeps its query between opens, so typing into it appends to
 * the last search. This sets the value and fires the input event the palette
 * listens for, as a person selecting the old text and typing over it would.
 */
export async function setPaletteQuery(text, timeout = 30_000) {
  await $(".palette-query").waitForExist({ timeout });
  await browser.execute((value) => {
    const query = document.querySelector(".palette-query");
    query.value = value;
    query.dispatchEvent(new Event("input", { bubbles: true }));
  }, text);
}

/**
 * What the webview asked the host for and what became of it, from the
 * desktop's own log, so a failure names the command that was refused rather
 * than only the element that never appeared.
 */
export function intentsSent() {
  try {
    const lines = run("sh", [
      "-c",
      'cat "$1"/desktop.log* 2>/dev/null | grep "renderer intent"',
      "log",
      env().AINB_HANGAR_HOME,
    ]);
    return lines
      .split("\n")
      .filter(Boolean)
      .map(
        (line) =>
          `${line.match(/command="?([^"\s]+)/)?.[1] ?? "unknown"}:${line.match(/outcome="?(\w+)/)?.[1] ?? "unknown"}`,
      );
  } catch {
    return [];
  }
}

/**
 * The settings category the frame says is selected, as the tree draws it.
 *
 * Read through the document rather than a `:has()` selector: this runner's
 * WebKit is the one the bundle ships with, not the newest one.
 */
export async function selectedNode() {
  return browser.execute(() => {
    const current = document.querySelector('.settings-node button[aria-current="true"]');
    return current === null ? null : current.closest(".settings-node").getAttribute("data-node");
  });
}

/**
 * Every batch the window applied, as the desktop logged it: one entry per
 * `renderer applied` line, with the sections it carried.
 *
 * A window that looks slow with a bounded DOM is usually busy with frames, and
 * this is the only view of that from outside the window.
 */
export function appliedBatches() {
  try {
    const lines = run("sh", [
      "-c",
      'cat "$1"/desktop.log* 2>/dev/null | grep "renderer applied"',
      "log",
      env().AINB_HANGAR_HOME,
    ]);
    return lines.split("\n").filter(Boolean);
  } catch {
    return [];
  }
}
