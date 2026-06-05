"""
Advanced Wait Strategies for Reliable Web Automation

Better alternatives to simple sleep() or networkidle for dynamic web applications.
"""

from playwright.sync_api import Page
import time
from typing import Callable, Optional, Any


def wait_for_api_call(page: Page, url_pattern: str, timeout_seconds: int = 10) -> Optional[Any]:
    """
    Wait for a specific API call to complete and return its response.

    Args:
        page: Playwright Page object
        url_pattern: URL pattern to match (can include wildcards)
        timeout_seconds: Maximum time to wait

    Returns:
        Response data if call completed, None if timeout

    Example:
        ```python
        # Wait for user profile API call
        response = wait_for_api_call(page, '**/api/profile**')
        if response:
            print(f"Profile loaded: {response}")
        ```
    """
    response_data = {'data': None, 'completed': False}

    def handle_response(response):
        if url_pattern.replace('**', '') in response.url:
            try:
                response_data['data'] = response.json()
                response_data['completed'] = True
            except:
                response_data['completed'] = True

    page.on('response', handle_response)

    start_time = time.time()
    while not response_data['completed'] and (time.time() - start_time) < timeout_seconds:
        time.sleep(0.1)

    page.remove_listener('response', handle_response)

    return response_data['data']


def wait_for_element_stable(page: Page, selector: str, stability_ms: int = 1000, timeout_seconds: int = 10) -> bool:
    """
    Wait for an element's position to stabilize (stop moving/changing).

    Useful for elements that animate or shift due to dynamic content loading.

    Args:
        page: Playwright Page object
        selector: CSS selector for the element
        stability_ms: Duration element must remain stable (milliseconds)
        timeout_seconds: Maximum time to wait

    Returns:
        True if element stabilized, False if timeout

    Example:
        ```python
        # Wait for dropdown menu to finish animating
        wait_for_element_stable(page, '.dropdown-menu', stability_ms=500)
        ```
    """
    try:
        element = page.locator(selector).first

        script = f"""
        (element, stabilityMs) => {{
            return new Promise((resolve) => {{
                let lastRect = element.getBoundingClientRect();
                let lastChange = Date.now();

                const checkStability = () => {{
                    const currentRect = element.getBoundingClientRect();

                    if (currentRect.top !== lastRect.top ||
                        currentRect.left !== lastRect.left ||
                        currentRect.width !== lastRect.width ||
                        currentRect.height !== lastRect.height) {{
                        lastChange = Date.now();
                        lastRect = currentRect;
                    }}

                    if (Date.now() - lastChange >= stabilityMs) {{
                        resolve(true);
                    }} else if (Date.now() - lastChange < {timeout_seconds * 1000}) {{
                        setTimeout(checkStability, 50);
                    }} else {{
                        resolve(false);
                    }}
                }};

                setTimeout(checkStability, stabilityMs);
            }});
        }}
        """

        result = element.evaluate(script, stability_ms)
        return result
    except:
        return False


def wait_with_retry(page: Page, condition_fn: Callable[[], bool], max_retries: int = 5, backoff_seconds: float = 0.5) -> bool:
    """
    Wait for a condition with exponential backoff retry.

    Args:
        page: Playwright Page object
        condition_fn: Function that returns True when condition is met
        max_retries: Maximum number of retry attempts
        backoff_seconds: Initial backoff duration (doubles each retry)

    Returns:
        True if condition met, False if all retries exhausted

    Example:
        ```python
        # Wait for specific element to appear with retry
        def check_dashboard():
            return page.locator('#dashboard').is_visible()

        if wait_with_retry(page, check_dashboard):
            print("Dashboard loaded!")
        ```
    """
    wait_time = backoff_seconds

    for attempt in range(max_retries):
        try:
            if condition_fn():
                return True
        except:
            pass

        if attempt < max_retries - 1:
            time.sleep(wait_time)
            wait_time *= 2  # Exponential backoff

    return False


def smart_navigation_wait(page: Page, expected_url_pattern: str = None, timeout_seconds: int = 10) -> bool:
    """
    Comprehensive wait strategy after navigation/interaction.

    Combines multiple strategies:
    1. Network idle
    2. DOM stability
    3. URL pattern match (if provided)

    Args:
        page: Playwright Page object
        expected_url_pattern: Optional URL pattern to wait for
        timeout_seconds: Maximum time to wait

    Returns:
        True if all conditions met, False if timeout

    Example:
        ```python
        page.click('button#login')
        smart_navigation_wait(page, expected_url_pattern='**/dashboard**')
        ```
    """
    start_time = time.time()

    # Step 1: Wait for network idle
    try:
        page.wait_for_load_state('networkidle', timeout=timeout_seconds * 1000)
    except:
        pass

    # Step 2: Check URL if pattern provided
    if expected_url_pattern:
        while (time.time() - start_time) < timeout_seconds:
            current_url = page.url
            pattern = expected_url_pattern.replace('**', '')
            if pattern in current_url:
                break
            time.sleep(0.5)
        else:
            return False

    # Step 3: Brief wait for DOM stability
    time.sleep(1)

    return True


def wait_for_data_load(page: Page, data_attribute: str = 'data-loaded', timeout_seconds: int = 10) -> bool:
    """
    Wait for data-loading attribute to indicate completion.

    Args:
        page: Playwright Page object
        data_attribute: Data attribute to check (e.g., 'data-loaded')
        timeout_seconds: Maximum time to wait

    Returns:
        True if data loaded, False if timeout

    Example:
        ```python
        # Wait for element with data-loaded="true"
        wait_for_data_load(page, data_attribute='data-loaded')
        ```
    """
    start_time = time.time()

    while (time.time() - start_time) < timeout_seconds:
        try:
            elements = page.locator(f'[{data_attribute}="true"]').all()
            if elements:
                return True
        except:
            pass

        time.sleep(0.3)

    return False


def wait_until_no_element(page: Page, selector: str, timeout_seconds: int = 10) -> bool:
    """
    Wait until an element is no longer visible (e.g., loading spinner disappears).

    Args:
        page: Playwright Page object
        selector: CSS selector for the element
        timeout_seconds: Maximum time to wait

    Returns:
        True if element disappeared, False if still visible after timeout

    Example:
        ```python
        # Wait for loading spinner to disappear
        wait_until_no_element(page, '.loading-spinner')
        ```
    """
    start_time = time.time()

    while (time.time() - start_time) < timeout_seconds:
        try:
            element = page.locator(selector).first
            if not element.is_visible(timeout=500):
                return True
        except:
            return True  # Element not found = disappeared

        time.sleep(0.3)

    return False


def combined_wait(page: Page, timeout_seconds: int = 10) -> bool:
    """
    Comprehensive wait combining multiple strategies for maximum reliability.

    Uses:
    1. Network idle
    2. No visible loading indicators
    3. DOM stability
    4. Brief settling time

    Args:
        page: Playwright Page object
        timeout_seconds: Maximum time to wait

    Returns:
        True if all conditions met, False if timeout

    Example:
        ```python
        page.click('button#submit')
        combined_wait(page)  # Wait for everything to settle
        ```
    """
    start_time = time.time()

    # Network idle
    try:
        page.wait_for_load_state('networkidle', timeout=timeout_seconds * 1000)
    except:
        pass

    # Wait for common loading indicators to disappear
    loading_selectors = [
        '.loading',
        '.spinner',
        '[data-loading="true"]',
        '[aria-busy="true"]',
    ]

    for selector in loading_selectors:
        wait_until_no_element(page, selector, timeout_seconds=3)

    # Final settling time
    time.sleep(1)

    return (time.time() - start_time) < timeout_seconds
