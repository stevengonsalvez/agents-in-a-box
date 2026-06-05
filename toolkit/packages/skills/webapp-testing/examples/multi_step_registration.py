#!/usr/bin/env python3
"""
Multi-Step Registration Example

Demonstrates complete registration flow using all webapp-testing utilities:
- UI interactions (cookie banners, modals)
- Smart form filling (handles field variations)
- Database operations (invite codes, email verification)
- Advanced wait strategies

This example is based on a real-world React/Supabase app with 3-step registration.
"""

import sys
import os
from playwright.sync_api import sync_playwright
import time

# Add utils to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from utils.ui_interactions import dismiss_cookie_banner, dismiss_modal
from utils.form_helpers import SmartFormFiller, handle_multi_step_form
from utils.supabase import SupabaseTestClient
from utils.wait_strategies import combined_wait, smart_navigation_wait


def register_user_complete_flow():
    """
    Complete multi-step registration with database setup and verification.

    Flow:
    1. Create invite code in database
    2. Navigate to registration page
    3. Fill multi-step form (Code → Credentials → Personal Info → Avatar)
    4. Verify email via database
    5. Login
    6. Verify dashboard access
    7. Cleanup (optional)
    """

    # Configuration - adjust for your app
    APP_URL = "http://localhost:3000"
    REGISTER_URL = f"{APP_URL}/register"

    # Database config (adjust for your project)
    DB_PASSWORD = "your-db-password"
    SUPABASE_URL = "https://project.supabase.co"
    SERVICE_KEY = "your-service-role-key"

    # Test user data
    TEST_EMAIL = "test.user@example.com"
    TEST_PASSWORD = "TestPass123!"
    FULL_NAME = "Test User"
    PHONE = "+447700900123"
    DATE_OF_BIRTH = "1990-01-15"
    INVITE_CODE = "TEST2024"

    print("\n" + "="*60)
    print("MULTI-STEP REGISTRATION AUTOMATION")
    print("="*60)

    # Step 1: Setup database
    print("\n[1/8] Setting up database...")
    db_client = SupabaseTestClient(
        url=SUPABASE_URL,
        service_key=SERVICE_KEY,
        db_password=DB_PASSWORD
    )

    # Create invite code
    if db_client.create_invite_code(INVITE_CODE, code_type="general"):
        print(f"    ✓ Created invite code: {INVITE_CODE}")
    else:
        print(f"    ⚠️  Invite code may already exist")

    # Clean up any existing test user
    existing_user = db_client.find_user_by_email(TEST_EMAIL)
    if existing_user:
        print(f"    Cleaning up existing user...")
        db_client.cleanup_related_records(existing_user)
        db_client.delete_user(existing_user)

    # Step 2: Start browser automation
    print("\n[2/8] Starting browser automation...")

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False)
        page = browser.new_page(viewport={'width': 1400, 'height': 1000})

        try:
            # Step 3: Navigate to registration
            print("\n[3/8] Navigating to registration page...")
            page.goto(REGISTER_URL, wait_until='networkidle')
            smart_navigation_wait(page)  # Use smart wait instead of time.sleep(2)

            # Handle cookie banner
            if dismiss_cookie_banner(page):
                print("    ✓ Dismissed cookie banner")

            page.screenshot(path='/tmp/reg_step1_start.png', full_page=True)
            print("    ✓ Screenshot: /tmp/reg_step1_start.png")

            # Step 4: Fill multi-step form
            print("\n[4/8] Filling multi-step registration form...")

            # Define form steps
            steps = [
                {
                    'name': 'Invite Code',
                    'fields': {'invite_code': INVITE_CODE},
                    'custom_fill': lambda: page.locator('input').first.fill(INVITE_CODE),
                    'custom_submit': lambda: page.locator('input').first.press('Enter'),
                },
                {
                    'name': 'Credentials',
                    'fields': {
                        'email': TEST_EMAIL,
                        'password': TEST_PASSWORD,
                    },
                    'checkbox': True,  # Terms of service
                },
                {
                    'name': 'Personal Info',
                    'fields': {
                        'full_name': FULL_NAME,
                        'date_of_birth': DATE_OF_BIRTH,
                        'phone': PHONE,
                    },
                },
                {
                    'name': 'Avatar Selection',
                    'complete': True,  # Final step with COMPLETE button
                }
            ]

            # Process each step
            filler = SmartFormFiller()

            for i, step in enumerate(steps):
                print(f"\n    Step {i+1}/4: {step['name']}")

                # Custom filling logic for first step (invite code)
                if 'custom_fill' in step:
                    step['custom_fill']()
                    combined_wait(page, timeout=2000)  # Brief wait for UI update

                    if 'custom_submit' in step:
                        step['custom_submit']()
                    else:
                        page.locator('button:has-text("CONTINUE")').first.click()

                    # Wait for navigation/API response
                    combined_wait(page, timeout=5000)
                    smart_navigation_wait(page)

                # Standard form filling for other steps
                elif 'fields' in step:
                    if 'email' in step['fields']:
                        filler.fill_email_field(page, step['fields']['email'])
                        print("      ✓ Email")

                    if 'password' in step['fields']:
                        filler.fill_password_fields(page, step['fields']['password'])
                        print("      ✓ Password")

                    if 'full_name' in step['fields']:
                        filler.fill_name_field(page, step['fields']['full_name'])
                        print("      ✓ Full Name")

                    if 'date_of_birth' in step['fields']:
                        filler.fill_date_field(page, step['fields']['date_of_birth'], field_hint='birth')
                        print("      ✓ Date of Birth")

                    if 'phone' in step['fields']:
                        filler.fill_phone_field(page, step['fields']['phone'])
                        print("      ✓ Phone")

                    # Check terms checkbox if needed
                    if step.get('checkbox'):
                        page.locator('input[type="checkbox"]').first.check()
                        print("      ✓ Terms accepted")

                    combined_wait(page, timeout=1000)  # Brief wait for form validation

                    # Click continue
                    page.locator('button:has-text("CONTINUE")').first.click()
                    combined_wait(page, timeout=5000)  # Wait for step transition
                    smart_navigation_wait(page)

                # Final step - click COMPLETE
                elif step.get('complete'):
                    complete_btn = page.locator('button:has-text("COMPLETE")').first
                    complete_btn.click()
                    print("      ✓ Clicked COMPLETE")

                    # Wait for registration to complete (may involve API calls)
                    combined_wait(page, timeout=10000)
                    smart_navigation_wait(page)

                # Screenshot after each step
                page.screenshot(path=f'/tmp/reg_step{i+1}_complete.png', full_page=True)
                print(f"      ✓ Screenshot: /tmp/reg_step{i+1}_complete.png")

            print("\n    ✓ Multi-step form completed!")

            # Step 5: Handle post-registration
            print("\n[5/8] Handling post-registration...")

            # Dismiss welcome modal if present
            if dismiss_modal(page, modal_identifier="Welcome"):
                print("    ✓ Dismissed welcome modal")

            current_url = page.url
            print(f"    Current URL: {current_url}")

            # Step 6: Verify email via database
            print("\n[6/8] Verifying email via database...")
            combined_wait(page, timeout=2000)  # Brief wait for user to be created in DB

            user_id = db_client.find_user_by_email(TEST_EMAIL)
            if user_id:
                print(f"    ✓ Found user: {user_id}")

                if db_client.confirm_email(user_id):
                    print("    ✓ Email verified in database")
                else:
                    print("    ⚠️  Could not verify email")
            else:
                print("    ⚠️  User not found in database")

            # Step 7: Login (if not already logged in)
            print("\n[7/8] Logging in...")

            if 'login' in current_url.lower():
                print("    Needs login...")

                filler.fill_email_field(page, TEST_EMAIL)
                filler.fill_password_fields(page, TEST_PASSWORD, confirm=False)
                combined_wait(page, timeout=1000)

                page.locator('button[type="submit"]').first.click()
                combined_wait(page, timeout=8000)  # Wait for login API
                smart_navigation_wait(page)

                print("    ✓ Logged in")
            else:
                print("    ✓ Already logged in")

            # Step 8: Verify dashboard access
            print("\n[8/8] Verifying dashboard access...")

            # Navigate to dashboard/perform if not already there
            if 'perform' not in page.url.lower() and 'dashboard' not in page.url.lower():
                page.goto(f"{APP_URL}/perform", wait_until='networkidle')
                smart_navigation_wait(page)

            page.screenshot(path='/tmp/reg_final_dashboard.png', full_page=True)
            print("    ✓ Screenshot: /tmp/reg_final_dashboard.png")

            # Check if we're on the dashboard
            if 'perform' in page.url.lower() or 'dashboard' in page.url.lower():
                print("    ✓ Successfully reached dashboard!")
            else:
                print(f"    ⚠️  Unexpected URL: {page.url}")

            print("\n" + "="*60)
            print("REGISTRATION COMPLETE!")
            print("="*60)
            print(f"\nUser: {TEST_EMAIL}")
            print(f"Password: {TEST_PASSWORD}")
            print(f"User ID: {user_id}")
            print(f"\nScreenshots saved to /tmp/reg_step*.png")
            print("="*60)

            # Keep browser open for inspection
            print("\nKeeping browser open for 30 seconds...")
            time.sleep(30)

        except Exception as e:
            print(f"\n❌ Error: {e}")
            import traceback
            traceback.print_exc()
            page.screenshot(path='/tmp/reg_error.png', full_page=True)
            print("    Error screenshot: /tmp/reg_error.png")

        finally:
            browser.close()

            # Optional cleanup
            print("\n" + "="*60)
            print("Cleanup")
            print("="*60)

            cleanup = input("\nDelete test user? (y/N): ").strip().lower()
            if cleanup == 'y' and user_id:
                print("Cleaning up...")
                db_client.cleanup_related_records(user_id)
                db_client.delete_user(user_id)
                print("✓ Test user deleted")
            else:
                print("Test user kept for manual testing")


if __name__ == '__main__':
    print("\nMulti-Step Registration Automation Example")
    print("=" * 60)
    print("\nBefore running:")
    print("1. Update configuration variables at the top of the script")
    print("2. Ensure your app is running (e.g., npm run dev)")
    print("3. Have database credentials ready")
    print("\n" + "=" * 60)

    proceed = input("\nProceed with registration? (y/N): ").strip().lower()

    if proceed == 'y':
        register_user_complete_flow()
    else:
        print("\nCancelled.")
