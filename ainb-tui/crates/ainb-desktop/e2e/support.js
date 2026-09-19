// Helpers every desktop journey spec shares, so a spec does not grow its own
// copy of how to click or how to type into the palette.

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
export async function click(selector, timeout = 60_000) {
  let last = null;
  await browser.waitUntil(
    async () => {
      try {
        const element = await $(selector);
        if (!(await element.isExisting())) return false;
        await element.click();
        return true;
      } catch (error) {
        last = error;
        if (!/stale element|no longer attached|not interactable/i.test(String(error))) throw error;
        return false;
      }
    },
    { timeout, timeoutMsg: () => `${selector} never took a click: ${last}` },
  );
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
