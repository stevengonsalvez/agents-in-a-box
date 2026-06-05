"""
UI Interaction Helpers for Web Automation

Common UI patterns that appear across many web applications:
- Cookie consent banners
- Modal dialogs
- Loading overlays
- Welcome tours/onboarding
- Fixed headers blocking clicks
"""

from playwright.sync_api import Page
import time


def dismiss_cookie_banner(page: Page, timeout: int = 3000) -> bool:
    """
    Detect and dismiss cookie consent banners.

    Tries common patterns:
    - "Accept" / "Accept All" / "OK" buttons
    - "I Agree" / "Got it" buttons
    - Cookie banner containers

    Args:
        page: Playwright Page object
        timeout: Maximum time to wait for banner (milliseconds)

    Returns:
        True if banner was found and dismissed, False otherwise

    Example:
        ```python
        page.goto('https://example.com')
        if dismiss_cookie_banner(page):
            print("Cookie banner dismissed")
        ```
    """
    cookie_button_selectors = [
        'button:has-text("Accept")',
        'button:has-text("Accept All")',
        'button:has-text("Accept all")',
        'button:has-text("I Agree")',
        'button:has-text("I agree")',
        'button:has-text("OK")',
        'button:has-text("Got it")',
        'button:has-text("Allow")',
        'button:has-text("Allow all")',
        '[data-testid="cookie-accept"]',
        '[data-testid="accept-cookies"]',
        '[id*="cookie-accept" i]',
        '[id*="accept-cookie" i]',
        '[class*="cookie-accept" i]',
    ]

    for selector in cookie_button_selectors:
        try:
            button = page.locator(selector).first
            if button.is_visible(timeout=timeout):
                button.click()
                time.sleep(0.5)  # Brief wait for banner to disappear
                return True
        except:
            continue

    return False


def dismiss_modal(page: Page, modal_identifier: str = None, timeout: int = 2000) -> bool:
    """
    Close modal dialogs with multiple fallback strategies.

    Strategies:
    1. If identifier provided, close that specific modal
    2. Click close button (X, Close, Cancel, etc.)
    3. Press Escape key
    4. Click backdrop/overlay

    Args:
        page: Playwright Page object
        modal_identifier: Optional - specific text in modal to identify it
        timeout: Maximum time to wait for modal (milliseconds)

    Returns:
        True if modal was found and closed, False otherwise

    Example:
        ```python
        # Close any modal
        dismiss_modal(page)

        # Close specific "Welcome" modal
        dismiss_modal(page, modal_identifier="Welcome")
        ```
    """
    # If specific modal identifier provided, wait for it first
    if modal_identifier:
        try:
            modal = page.locator(f'[role="dialog"]:has-text("{modal_identifier}"), dialog:has-text("{modal_identifier}")').first
            if not modal.is_visible(timeout=timeout):
                return False
        except:
            return False

    # Strategy 1: Click close button
    close_button_selectors = [
        'button:has-text("Close")',
        'button:has-text("×")',
        'button:has-text("X")',
        'button:has-text("Cancel")',
        'button:has-text("GOT IT")',
        'button:has-text("Got it")',
        'button:has-text("OK")',
        'button:has-text("Dismiss")',
        '[aria-label="Close"]',
        '[aria-label="close"]',
        '[data-testid="close-modal"]',
        '[class*="close" i]',
        '[class*="dismiss" i]',
    ]

    for selector in close_button_selectors:
        try:
            button = page.locator(selector).first
            if button.is_visible(timeout=500):
                button.click()
                time.sleep(0.5)
                return True
        except:
            continue

    # Strategy 2: Press Escape key
    try:
        page.keyboard.press('Escape')
        time.sleep(0.5)
        # Check if modal is gone
        modals = page.locator('[role="dialog"], dialog').all()
        if all(not m.is_visible() for m in modals):
            return True
    except:
        pass

    # Strategy 3: Click backdrop (if exists and clickable)
    try:
        backdrop = page.locator('[class*="backdrop" i], [class*="overlay" i]').first
        if backdrop.is_visible(timeout=500):
            backdrop.click(position={'x': 10, 'y': 10})  # Click corner, not center
            time.sleep(0.5)
            return True
    except:
        pass

    return False


def click_with_header_offset(page: Page, selector: str, header_height: int = 80, force: bool = False):
    """
    Click an element while accounting for fixed headers that might block it.

    Scrolls the element into view with an offset to avoid fixed headers,
    then clicks it.

    Args:
        page: Playwright Page object
        selector: CSS selector for the element to click
        header_height: Height of fixed header in pixels (default 80)
        force: Whether to use force click if normal click fails

    Example:
        ```python
        # Click button that might be behind a fixed header
        click_with_header_offset(page, 'button#submit', header_height=100)
        ```
    """
    element = page.locator(selector).first

    # Scroll element into view with offset
    element.evaluate(f'el => el.scrollIntoView({{ block: "center", inline: "nearest" }})')
    page.evaluate(f'window.scrollBy(0, -{header_height})')
    time.sleep(0.3)  # Brief wait for scroll to complete

    try:
        element.click()
    except Exception as e:
        if force:
            element.click(force=True)
        else:
            raise e


def force_click_if_needed(page: Page, selector: str, timeout: int = 5000) -> bool:
    """
    Try normal click first, use force click if it fails (e.g., due to overlays).

    Args:
        page: Playwright Page object
        selector: CSS selector for the element to click
        timeout: Maximum time to wait for element (milliseconds)

    Returns:
        True if click succeeded (normal or forced), False otherwise

    Example:
        ```python
        # Try to click, handling potential overlays
        if force_click_if_needed(page, 'button#submit'):
            print("Button clicked successfully")
        ```
    """
    try:
        element = page.locator(selector).first
        if not element.is_visible(timeout=timeout):
            return False

        # Try normal click first
        try:
            element.click(timeout=timeout)
            return True
        except:
            # Fall back to force click
            element.click(force=True)
            return True
    except:
        return False


def wait_for_no_overlay(page: Page, max_wait_seconds: int = 10) -> bool:
    """
    Wait for loading overlays/spinners to disappear.

    Looks for common loading overlay patterns and waits until they're gone.

    Args:
        page: Playwright Page object
        max_wait_seconds: Maximum time to wait (seconds)

    Returns:
        True if overlays disappeared, False if timeout

    Example:
        ```python
        page.click('button#submit')
        wait_for_no_overlay(page)  # Wait for loading to complete
        ```
    """
    overlay_selectors = [
        '[class*="loading" i]',
        '[class*="spinner" i]',
        '[class*="overlay" i]',
        '[class*="backdrop" i]',
        '[data-loading="true"]',
        '[aria-busy="true"]',
        '.loader',
        '.loading',
        '#loading',
    ]

    start_time = time.time()

    while time.time() - start_time < max_wait_seconds:
        all_hidden = True

        for selector in overlay_selectors:
            try:
                overlays = page.locator(selector).all()
                for overlay in overlays:
                    if overlay.is_visible():
                        all_hidden = False
                        break
            except:
                continue

            if not all_hidden:
                break

        if all_hidden:
            return True

        time.sleep(0.5)

    return False


def handle_welcome_tour(page: Page, skip_button_text: str = "Skip") -> bool:
    """
    Automatically skip onboarding tours or welcome wizards.

    Looks for and clicks "Skip", "Skip Tour", "Close", "Maybe Later" buttons.

    Args:
        page: Playwright Page object
        skip_button_text: Text to look for in skip buttons (default "Skip")

    Returns:
        True if tour was skipped, False if no tour found

    Example:
        ```python
        page.goto('https://app.example.com')
        handle_welcome_tour(page)  # Skip any onboarding tour
        ```
    """
    skip_selectors = [
        f'button:has-text("{skip_button_text}")',
        'button:has-text("Skip Tour")',
        'button:has-text("Maybe Later")',
        'button:has-text("No Thanks")',
        'button:has-text("Close Tour")',
        '[data-testid="skip-tour"]',
        '[data-testid="close-tour"]',
        '[aria-label="Skip tour"]',
        '[aria-label="Close tour"]',
    ]

    for selector in skip_selectors:
        try:
            button = page.locator(selector).first
            if button.is_visible(timeout=2000):
                button.click()
                time.sleep(0.5)
                return True
        except:
            continue

    return False


def wait_for_stable_dom(page: Page, stability_duration_ms: int = 1000, max_wait_seconds: int = 10) -> bool:
    """
    Wait for the DOM to stop changing (useful for dynamic content loading).

    Monitors for DOM mutations and waits until no changes occur for the specified duration.

    Args:
        page: Playwright Page object
        stability_duration_ms: Duration of no changes to consider stable (milliseconds)
        max_wait_seconds: Maximum time to wait (seconds)

    Returns:
        True if DOM stabilized, False if timeout

    Example:
        ```python
        page.goto('https://app.example.com')
        wait_for_stable_dom(page)  # Wait for all dynamic content to load
        ```
    """
    # Inject mutation observer script
    script = f"""
    new Promise((resolve) => {{
        let lastMutation = Date.now();
        const observer = new MutationObserver(() => {{
            lastMutation = Date.now();
        }});

        observer.observe(document.body, {{
            childList: true,
            subtree: true,
            attributes: true
        }});

        const checkStability = () => {{
            if (Date.now() - lastMutation >= {stability_duration_ms}) {{
                observer.disconnect();
                resolve(true);
            }} else if (Date.now() - lastMutation > {max_wait_seconds * 1000}) {{
                observer.disconnect();
                resolve(false);
            }} else {{
                setTimeout(checkStability, 100);
            }}
        }};

        setTimeout(checkStability, {stability_duration_ms});
    }})
    """

    try:
        result = page.evaluate(script)
        return result
    except:
        return False


def setup_page_with_csp_handling(page):
    """
    Set up page with automatic CSP error detection and helpful suggestions.

    Monitors console for CSP violations and suggests fixes.
    Call this once per page before navigation.

    Args:
        page: Playwright Page object

    Example:
        setup_page_with_csp_handling(page)
        page.goto('http://localhost:7160')
        # If CSP violation occurs:
        # Output: ⚠️  CSP Violation detected: [error message]
        #         💡 Suggestion: Use BrowserConfig.create_test_context() with auto CSP bypass

    Usage with BrowserConfig:
        from utils.browser_config import BrowserConfig

        context = BrowserConfig.create_test_context(browser, 'http://localhost:3000')
        page = context.new_page()
        setup_page_with_csp_handling(page)
    """
    csp_violations = []

    def handle_console(msg):
        """Handler for console messages that detects CSP violations"""
        text = msg.text.lower()

        # Detect various CSP-related errors
        csp_keywords = [
            'content security policy',
            'csp',
            'refused to execute inline script',
            'refused to load',
            'blocked by content security policy',
        ]

        if any(keyword in text for keyword in csp_keywords):
            # Avoid duplicate messages
            if msg.text not in csp_violations:
                csp_violations.append(msg.text)
                print("\n" + "=" * 70)
                print("⚠️  CSP VIOLATION DETECTED")
                print("=" * 70)
                print(f"Message: {msg.text[:200]}")
                print("\n💡 SUGGESTION:")
                print("   For localhost testing, use:")
                print("   ")
                print("   from utils.browser_config import BrowserConfig")
                print("   context = BrowserConfig.create_test_context(")
                print("       browser, 'http://localhost:3000'")
                print("   )")
                print("   # Auto-enables CSP bypass for localhost")
                print("\n   Or manually:")
                print("   context = browser.new_context(bypass_csp=True)")
                print("=" * 70 + "\n")

    # Attach console listener
    page.on('console', handle_console)

    # Also monitor for page errors related to CSP
    def handle_page_error(error):
        """Handler for page errors"""
        error_text = str(error).lower()
        if 'content security policy' in error_text or 'csp' in error_text:
            print(f"\n⚠️  Page Error (CSP-related): {error}\n")

    page.on('pageerror', handle_page_error)
