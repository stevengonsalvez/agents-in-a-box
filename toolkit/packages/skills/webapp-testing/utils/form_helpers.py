"""
Smart Form Filling Helpers

Handles common form patterns across web applications:
- Multi-step forms with validation
- Dynamic field variations (full name vs first/last name)
- Retry strategies for flaky selectors
- Intelligent field detection
"""

from playwright.sync_api import Page
from typing import Dict, List, Any, Optional
import time


class SmartFormFiller:
    """
    Intelligent form filling that handles variations in field structures.

    Example:
        ```python
        filler = SmartFormFiller()
        filler.fill_name_field(page, "John Doe")  # Tries full name or first/last
        filler.fill_email_field(page, "test@example.com")
        filler.fill_password_fields(page, "SecurePass123!")
        ```
    """

    @staticmethod
    def fill_name_field(page: Page, full_name: str, timeout: int = 5000) -> bool:
        """
        Fill name field(s) - handles both single "Full Name" and separate "First/Last Name" fields.

        Args:
            page: Playwright Page object
            full_name: Full name as string (e.g., "John Doe")
            timeout: Maximum time to wait for fields (milliseconds)

        Returns:
            True if successful, False otherwise

        Example:
            ```python
            # Works with both field structures:
            # - Single field: "Full Name"
            # - Separate fields: "First Name" and "Last Name"
            fill_name_field(page, "Jane Smith")
            ```
        """
        # Strategy 1: Try single "Full Name" field
        full_name_selectors = [
            'input[name*="full" i][name*="name" i]',
            'input[placeholder*="full name" i]',
            'input[placeholder*="name" i]',
            'input[id*="fullname" i]',
            'input[id*="full-name" i]',
        ]

        for selector in full_name_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(full_name)
                    return True
            except:
                continue

        # Strategy 2: Try separate First/Last Name fields
        parts = full_name.split(' ', 1)
        first_name = parts[0] if parts else full_name
        last_name = parts[1] if len(parts) > 1 else ''

        first_name_selectors = [
            'input[name*="first" i][name*="name" i]',
            'input[placeholder*="first name" i]',
            'input[id*="firstname" i]',
            'input[id*="first-name" i]',
        ]

        last_name_selectors = [
            'input[name*="last" i][name*="name" i]',
            'input[placeholder*="last name" i]',
            'input[id*="lastname" i]',
            'input[id*="last-name" i]',
        ]

        first_filled = False
        last_filled = False

        # Fill first name
        for selector in first_name_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(first_name)
                    first_filled = True
                    break
            except:
                continue

        # Fill last name
        for selector in last_name_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(last_name)
                    last_filled = True
                    break
            except:
                continue

        return first_filled or last_filled

    @staticmethod
    def fill_email_field(page: Page, email: str, timeout: int = 5000) -> bool:
        """
        Fill email field with multiple selector strategies.

        Args:
            page: Playwright Page object
            email: Email address
            timeout: Maximum time to wait for field (milliseconds)

        Returns:
            True if successful, False otherwise
        """
        email_selectors = [
            'input[type="email"]',
            'input[name="email" i]',
            'input[placeholder*="email" i]',
            'input[id*="email" i]',
            'input[autocomplete="email"]',
        ]

        for selector in email_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(email)
                    return True
            except:
                continue

        return False

    @staticmethod
    def fill_password_fields(page: Page, password: str, confirm: bool = True, timeout: int = 5000) -> bool:
        """
        Fill password field(s) - handles both single password and password + confirm.

        Args:
            page: Playwright Page object
            password: Password string
            confirm: Whether to also fill confirmation field (default True)
            timeout: Maximum time to wait for fields (milliseconds)

        Returns:
            True if successful, False otherwise
        """
        password_fields = page.locator('input[type="password"]').all()

        if not password_fields:
            return False

        # Fill first password field
        try:
            password_fields[0].fill(password)
        except:
            return False

        # Fill confirmation field if requested and exists
        if confirm and len(password_fields) > 1:
            try:
                password_fields[1].fill(password)
            except:
                pass

        return True

    @staticmethod
    def fill_phone_field(page: Page, phone: str, timeout: int = 5000) -> bool:
        """
        Fill phone number field with multiple selector strategies.

        Args:
            page: Playwright Page object
            phone: Phone number string
            timeout: Maximum time to wait for field (milliseconds)

        Returns:
            True if successful, False otherwise
        """
        phone_selectors = [
            'input[type="tel"]',
            'input[name*="phone" i]',
            'input[placeholder*="phone" i]',
            'input[id*="phone" i]',
            'input[autocomplete="tel"]',
        ]

        for selector in phone_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(phone)
                    return True
            except:
                continue

        return False

    @staticmethod
    def fill_date_field(page: Page, date_value: str, field_hint: str = None, timeout: int = 5000) -> bool:
        """
        Fill date field (handles both date input and text input).

        Args:
            page: Playwright Page object
            date_value: Date as string (format: YYYY-MM-DD for date inputs)
            field_hint: Optional hint about field (e.g., "birth", "start", "end")
            timeout: Maximum time to wait for field (milliseconds)

        Returns:
            True if successful, False otherwise

        Example:
            ```python
            fill_date_field(page, "1990-01-15", field_hint="birth")
            ```
        """
        # Build selectors based on hint
        date_selectors = ['input[type="date"]']

        if field_hint:
            date_selectors.extend([
                f'input[name*="{field_hint}" i]',
                f'input[placeholder*="{field_hint}" i]',
                f'input[id*="{field_hint}" i]',
            ])

        for selector in date_selectors:
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(date_value)
                    return True
            except:
                continue

        return False


def fill_with_retry(page: Page, selectors: List[str], value: str, max_attempts: int = 3) -> bool:
    """
    Try multiple selectors with retry logic.

    Args:
        page: Playwright Page object
        selectors: List of CSS selectors to try
        value: Value to fill
        max_attempts: Maximum retry attempts per selector

    Returns:
        True if any selector succeeded, False otherwise

    Example:
        ```python
        selectors = ['input#email', 'input[name="email"]', 'input[type="email"]']
        fill_with_retry(page, selectors, 'test@example.com')
        ```
    """
    for selector in selectors:
        for attempt in range(max_attempts):
            try:
                field = page.locator(selector).first
                if field.is_visible(timeout=1000):
                    field.fill(value)
                    time.sleep(0.3)
                    # Verify value was set
                    if field.input_value() == value:
                        return True
            except:
                if attempt < max_attempts - 1:
                    time.sleep(0.5)
                continue

    return False


def handle_multi_step_form(page: Page, steps: List[Dict[str, Any]], continue_button_text: str = "CONTINUE") -> bool:
    """
    Automate multi-step form completion.

    Args:
        page: Playwright Page object
        steps: List of step configurations, each with fields and actions
        continue_button_text: Text of button to advance steps

    Returns:
        True if all steps completed successfully, False otherwise

    Example:
        ```python
        steps = [
            {
                'fields': {'email': 'test@example.com', 'password': 'Pass123!'},
                'checkbox': 'terms',  # Optional checkbox to check
                'wait_after': 2,  # Optional wait time after step
            },
            {
                'fields': {'full_name': 'John Doe', 'date_of_birth': '1990-01-15'},
            },
            {
                'complete': True,  # Final step, click complete/finish button
            }
        ]
        handle_multi_step_form(page, steps)
        ```
    """
    filler = SmartFormFiller()

    for i, step in enumerate(steps):
        print(f"  Processing step {i+1}/{len(steps)}...")

        # Fill fields in this step
        if 'fields' in step:
            for field_type, value in step['fields'].items():
                if field_type == 'email':
                    filler.fill_email_field(page, value)
                elif field_type == 'password':
                    filler.fill_password_fields(page, value)
                elif field_type == 'full_name':
                    filler.fill_name_field(page, value)
                elif field_type == 'phone':
                    filler.fill_phone_field(page, value)
                elif field_type.startswith('date'):
                    hint = field_type.replace('date_', '').replace('_', ' ')
                    filler.fill_date_field(page, value, field_hint=hint)
                else:
                    # Generic field - try to find and fill
                    print(f"    Warning: Unknown field type '{field_type}'")

        # Check checkbox if specified
        if 'checkbox' in step:
            try:
                checkbox = page.locator('input[type="checkbox"]').first
                checkbox.check()
            except:
                print(f"    Warning: Could not check checkbox")

        # Wait if specified
        if 'wait_after' in step:
            time.sleep(step['wait_after'])
        else:
            time.sleep(1)

        # Click continue/submit button
        if i < len(steps) - 1:  # Not the last step
            button_selectors = [
                f'button:has-text("{continue_button_text}")',
                'button[type="submit"]',
                'button:has-text("Next")',
                'button:has-text("Continue")',
            ]

            clicked = False
            for selector in button_selectors:
                try:
                    button = page.locator(selector).first
                    if button.is_visible(timeout=2000):
                        button.click()
                        clicked = True
                        break
                except:
                    continue

            if not clicked:
                print(f"    Warning: Could not find continue button for step {i+1}")
                return False

            # Wait for next step to load
            page.wait_for_load_state('networkidle')
            time.sleep(2)

        else:  # Last step
            if step.get('complete', False):
                complete_selectors = [
                    'button:has-text("COMPLETE")',
                    'button:has-text("Complete")',
                    'button:has-text("FINISH")',
                    'button:has-text("Finish")',
                    'button:has-text("SUBMIT")',
                    'button:has-text("Submit")',
                    'button[type="submit"]',
                ]

                for selector in complete_selectors:
                    try:
                        button = page.locator(selector).first
                        if button.is_visible(timeout=2000):
                            button.click()
                            page.wait_for_load_state('networkidle')
                            time.sleep(3)
                            return True
                    except:
                        continue

                print("    Warning: Could not find completion button")
                return False

    return True


def auto_fill_form(page: Page, field_mapping: Dict[str, str]) -> Dict[str, bool]:
    """
    Automatically fill a form based on field mapping.

    Intelligently detects field types and uses appropriate filling strategies.

    Args:
        page: Playwright Page object
        field_mapping: Dictionary mapping field types to values

    Returns:
        Dictionary with results for each field (True = filled, False = failed)

    Example:
        ```python
        results = auto_fill_form(page, {
            'email': 'test@example.com',
            'password': 'SecurePass123!',
            'full_name': 'Jane Doe',
            'phone': '+447700900123',
            'date_of_birth': '1990-01-15',
        })
        print(f"Email filled: {results['email']}")
        ```
    """
    filler = SmartFormFiller()
    results = {}

    for field_type, value in field_mapping.items():
        if field_type == 'email':
            results[field_type] = filler.fill_email_field(page, value)
        elif field_type == 'password':
            results[field_type] = filler.fill_password_fields(page, value)
        elif 'name' in field_type.lower():
            results[field_type] = filler.fill_name_field(page, value)
        elif 'phone' in field_type.lower():
            results[field_type] = filler.fill_phone_field(page, value)
        elif 'date' in field_type.lower():
            hint = field_type.replace('date_of_', '').replace('_', ' ')
            results[field_type] = filler.fill_date_field(page, value, field_hint=hint)
        else:
            # Try generic fill
            try:
                field = page.locator(f'input[name="{field_type}"]').first
                field.fill(value)
                results[field_type] = True
            except:
                results[field_type] = False

    return results
