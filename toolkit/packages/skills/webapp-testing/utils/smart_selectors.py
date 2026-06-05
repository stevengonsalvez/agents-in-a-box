"""
Smart Selector Strategies for Robust Web Testing

Automatically tries multiple selector strategies to find elements,
reducing test brittleness when HTML structure changes.

Usage:
    from utils.smart_selectors import SelectorStrategies

    # Find and fill email field
    SelectorStrategies.smart_fill(page, 'email', 'test@example.com')

    # Find and click button
    SelectorStrategies.smart_click(page, 'Sign In')
"""

from typing import Optional, List
from playwright.sync_api import Page, TimeoutError as PlaywrightTimeoutError


class SelectorStrategies:
    """Multiple strategies for finding common elements"""

    # Reduced timeouts for faster failure (5s per strategy instead of default 30s)
    DEFAULT_TIMEOUT = 5000  # 5 seconds per strategy attempt
    MAX_TOTAL_TIMEOUT = 10000  # 10 seconds max across all strategies

    @staticmethod
    def find_input_field(
        page: Page,
        field_type: str,
        timeout: int = DEFAULT_TIMEOUT,
        verbose: bool = True
    ) -> Optional[str]:
        """
        Find input field using multiple strategies in order of reliability.

        Strategies (in order):
        1. Test IDs: [data-testid*="field_type"]
        2. ARIA labels: input[aria-label*="field_type" i]
        3. Placeholder: input[placeholder*="field_type" i]
        4. Name attribute: input[name*="field_type" i]
        5. Type attribute: input[type="field_type"]
        6. ID attribute: #field_type, input[id*="field_type" i]

        Args:
            page: Playwright Page object
            field_type: Type of field to find (e.g., 'email', 'password')
            timeout: Timeout per strategy in milliseconds (default: 5000)
            verbose: Print which strategy succeeded (default: True)

        Returns:
            Selector string that worked, or None if not found

        Example:
            selector = SelectorStrategies.find_input_field(page, 'email')
            if selector:
                page.fill(selector, 'test@example.com')
        """
        strategies = [
            # Strategy 1: Test IDs (most reliable)
            (f'[data-testid*="{field_type}" i]', 'data-testid'),

            # Strategy 2: ARIA labels (accessibility best practice)
            (f'input[aria-label*="{field_type}" i]', 'aria-label'),

            # Strategy 3: Placeholder text
            (f'input[placeholder*="{field_type}" i]', 'placeholder'),

            # Strategy 4: Name attribute
            (f'input[name*="{field_type}" i]', 'name attribute'),

            # Strategy 5: Type attribute (works for email, password, text)
            (f'input[type="{field_type}"]', 'type attribute'),

            # Strategy 6: ID attribute (exact match)
            (f'#{field_type}', 'id (exact)'),

            # Strategy 7: ID attribute (partial match)
            (f'input[id*="{field_type}" i]', 'id (partial)'),
        ]

        for selector, strategy_name in strategies:
            try:
                locator = page.locator(selector).first
                if locator.is_visible(timeout=timeout):
                    if verbose:
                        print(f"✓ Found field via {strategy_name}: {selector}")
                    return selector
            except PlaywrightTimeoutError:
                continue
            except Exception:
                # Catch other errors (element not found, etc.)
                continue

        if verbose:
            print(f"✗ Could not find field for '{field_type}' using any strategy")
        return None

    @staticmethod
    def find_button(
        page: Page,
        button_text: str,
        timeout: int = DEFAULT_TIMEOUT,
        verbose: bool = True
    ) -> Optional[str]:
        """
        Find button by text using multiple strategies.

        Strategies:
        1. Test ID: [data-testid*="button-text"]
        2. Role with name: button[name="button_text"]
        3. Exact text: button:has-text("Button Text")
        4. Partial text (case-insensitive): button:text-matches("button text", "i")
        5. Link as button: a:has-text("Button Text")
        6. Input submit: input[type="submit"][value*="button text" i]

        Args:
            page: Playwright Page object
            button_text: Text on the button
            timeout: Timeout per strategy in milliseconds
            verbose: Print which strategy succeeded

        Returns:
            Selector string that worked, or None if not found

        Example:
            selector = SelectorStrategies.find_button(page, 'Sign In')
            if selector:
                page.click(selector)
        """
        # Normalize button text for test-id matching
        test_id = button_text.lower().replace(' ', '-')

        strategies = [
            # Strategy 1: Test IDs
            (f'[data-testid*="{test_id}" i]', 'data-testid'),

            # Strategy 2: Button with name attribute
            (f'button[name*="{button_text}" i]', 'button name'),

            # Strategy 3: Exact text match
            (f'button:has-text("{button_text}")', 'exact text'),

            # Strategy 4: Case-insensitive text match
            (f'button:text-matches("{button_text}", "i")', 'case-insensitive text'),

            # Strategy 5: Link styled as button
            (f'a:has-text("{button_text}")', 'link (exact text)'),

            # Strategy 6: Link case-insensitive
            (f'a:text-matches("{button_text}", "i")', 'link (case-insensitive)'),

            # Strategy 7: Input submit button
            (f'input[type="submit"][value*="{button_text}" i]', 'submit input'),

            # Strategy 8: Any clickable element with text
            (f'[role="button"]:has-text("{button_text}")', 'role=button'),
        ]

        for selector, strategy_name in strategies:
            try:
                locator = page.locator(selector).first
                if locator.is_visible(timeout=timeout):
                    if verbose:
                        print(f"✓ Found button via {strategy_name}: {selector}")
                    return selector
            except PlaywrightTimeoutError:
                continue
            except Exception:
                continue

        if verbose:
            print(f"✗ Could not find button '{button_text}' using any strategy")
        return None

    @staticmethod
    def smart_fill(
        page: Page,
        field_type: str,
        value: str,
        timeout: int = MAX_TOTAL_TIMEOUT,
        verbose: bool = True
    ) -> bool:
        """
        Find and fill a field automatically using smart selector strategies.

        Args:
            page: Playwright Page object
            field_type: Type of field (e.g., 'email', 'password', 'username')
            value: Value to fill
            timeout: Max timeout across all strategies
            verbose: Print progress messages

        Returns:
            True if successful, False otherwise

        Example:
            success = SelectorStrategies.smart_fill(page, 'email', 'test@example.com')
            if not success:
                print("Failed to fill email field")
        """
        selector = SelectorStrategies.find_input_field(
            page, field_type, timeout=timeout // 2, verbose=verbose
        )

        if selector:
            try:
                page.fill(selector, value)
                if verbose:
                    print(f"✓ Filled '{field_type}' with value")
                return True
            except Exception as e:
                if verbose:
                    print(f"✗ Found field but failed to fill: {e}")
                return False

        return False

    @staticmethod
    def smart_click(
        page: Page,
        button_text: str,
        timeout: int = MAX_TOTAL_TIMEOUT,
        verbose: bool = True
    ) -> bool:
        """
        Find and click a button automatically using smart selector strategies.

        Args:
            page: Playwright Page object
            button_text: Text on the button to click
            timeout: Max timeout across all strategies
            verbose: Print progress messages

        Returns:
            True if successful, False otherwise

        Example:
            success = SelectorStrategies.smart_click(page, 'Sign In')
            if not success:
                print("Failed to click Sign In button")
        """
        selector = SelectorStrategies.find_button(
            page, button_text, timeout=timeout // 2, verbose=verbose
        )

        if selector:
            try:
                page.click(selector)
                if verbose:
                    print(f"✓ Clicked '{button_text}' button")
                return True
            except Exception as e:
                if verbose:
                    print(f"✗ Found button but failed to click: {e}")
                return False

        return False

    @staticmethod
    def find_any_element(
        page: Page,
        selectors: List[str],
        timeout: int = DEFAULT_TIMEOUT,
        verbose: bool = True
    ) -> Optional[str]:
        """
        Try multiple custom selectors and return the first one that works.

        Useful when you have specific selectors to try but want fallback logic.

        Args:
            page: Playwright Page object
            selectors: List of CSS selectors to try
            timeout: Timeout per selector
            verbose: Print which selector worked

        Returns:
            First selector that found a visible element, or None

        Example:
            selectors = [
                'button#submit',
                'button.submit-btn',
                'input[type="submit"]'
            ]
            selector = SelectorStrategies.find_any_element(page, selectors)
            if selector:
                page.click(selector)
        """
        for selector in selectors:
            try:
                locator = page.locator(selector).first
                if locator.is_visible(timeout=timeout):
                    if verbose:
                        print(f"✓ Found element: {selector}")
                    return selector
            except PlaywrightTimeoutError:
                continue
            except Exception:
                continue

        if verbose:
            print(f"✗ Could not find element using any of {len(selectors)} selectors")
        return None
