"""
Smart Browser Configuration for Testing

Automatically configures browser context with appropriate settings
based on the testing environment (localhost vs production).

Usage:
    from utils.browser_config import BrowserConfig

    context = BrowserConfig.create_test_context(
        browser,
        'http://localhost:3000'
    )
"""

from typing import Optional, Dict
from playwright.sync_api import Browser, BrowserContext
from urllib.parse import urlparse


class BrowserConfig:
    """Smart browser configuration for testing environments"""

    @staticmethod
    def is_localhost_url(url: str) -> bool:
        """
        Check if URL is localhost or local development environment.

        Args:
            url: URL to check

        Returns:
            True if localhost/127.0.0.1, False otherwise

        Examples:
            >>> BrowserConfig.is_localhost_url('http://localhost:3000')
            True
            >>> BrowserConfig.is_localhost_url('http://127.0.0.1:8080')
            True
            >>> BrowserConfig.is_localhost_url('https://production.com')
            False
        """
        try:
            parsed = urlparse(url)
            hostname = parsed.hostname or parsed.netloc

            localhost_patterns = [
                'localhost',
                '127.0.0.1',
                '0.0.0.0',
                '::1',  # IPv6 localhost
            ]

            return any(pattern in hostname.lower() for pattern in localhost_patterns)
        except Exception:
            return False

    @staticmethod
    def create_test_context(
        browser: Browser,
        base_url: str = 'http://localhost',
        bypass_csp: Optional[bool] = None,
        ignore_https_errors: bool = True,
        extra_http_headers: Optional[Dict[str, str]] = None,
        viewport: Optional[Dict[str, int]] = None,
        record_video: bool = False,
        verbose: bool = True
    ) -> BrowserContext:
        """
        Create browser context optimized for testing.

        Auto-detects CSP bypass need:
        - If base_url contains 'localhost' or '127.0.0.1' → bypass_csp=True
        - Otherwise → bypass_csp=False

        Args:
            browser: Playwright browser instance
            base_url: Base URL of application under test
            bypass_csp: Override auto-detection (None = auto-detect)
            ignore_https_errors: Ignore HTTPS errors (self-signed certs)
            extra_http_headers: Additional HTTP headers to send
            viewport: Custom viewport size (default: 1280x720)
            record_video: Record video of test session
            verbose: Print configuration choices

        Returns:
            Configured browser context

        Example:
            # Auto-detect CSP bypass for localhost
            context = BrowserConfig.create_test_context(
                browser,
                'http://localhost:7160'
            )
            # Output: 🔓 CSP bypass enabled (testing on localhost)

            # Manually override for production testing
            context = BrowserConfig.create_test_context(
                browser,
                'https://production.com',
                bypass_csp=False
            )
        """
        # Auto-detect CSP bypass if not specified
        if bypass_csp is None:
            bypass_csp = BrowserConfig.is_localhost_url(base_url)

        # Default viewport for consistent testing
        if viewport is None:
            viewport = {'width': 1280, 'height': 720}

        # Build context options
        context_options = {
            'bypass_csp': bypass_csp,
            'ignore_https_errors': ignore_https_errors,
            'viewport': viewport,
        }

        # Add extra headers if provided
        if extra_http_headers:
            context_options['extra_http_headers'] = extra_http_headers

        # Add video recording if requested
        if record_video:
            context_options['record_video_dir'] = '/tmp/playwright-videos'

        # Create context
        context = browser.new_context(**context_options)

        # Print configuration for visibility
        if verbose:
            print("\n" + "=" * 60)
            print("  Browser Context Configuration")
            print("=" * 60)
            print(f"  Base URL: {base_url}")

            if bypass_csp:
                print("  🔓 CSP bypass: ENABLED (testing on localhost)")
            else:
                print("  🔒 CSP bypass: DISABLED (production mode)")

            if ignore_https_errors:
                print("  ⚠️  HTTPS errors: IGNORED (self-signed certs OK)")

            print(f"  📐 Viewport: {viewport['width']}x{viewport['height']}")

            if extra_http_headers:
                print(f"  📨 Extra headers: {len(extra_http_headers)} header(s)")

            if record_video:
                print("  🎥 Video recording: ENABLED")

            print("=" * 60 + "\n")

        return context

    @staticmethod
    def create_mobile_context(
        browser: Browser,
        device: str = 'iPhone 12',
        base_url: str = 'http://localhost',
        bypass_csp: Optional[bool] = None,
        verbose: bool = True
    ) -> BrowserContext:
        """
        Create mobile browser context with device emulation.

        Args:
            browser: Playwright browser instance
            device: Device to emulate (e.g., 'iPhone 12', 'Pixel 5')
            base_url: Base URL of application under test
            bypass_csp: Override auto-detection
            verbose: Print configuration

        Returns:
            Mobile browser context

        Example:
            context = BrowserConfig.create_mobile_context(
                browser,
                device='iPhone 12',
                base_url='http://localhost:3000'
            )
        """
        from playwright.sync_api import devices

        # Get device descriptor
        if device not in devices:
            available = ', '.join(list(devices.keys())[:5])
            raise ValueError(
                f"Unknown device: {device}. "
                f"Available: {available}, ..."
            )

        device_descriptor = devices[device]

        # Auto-detect CSP bypass
        if bypass_csp is None:
            bypass_csp = BrowserConfig.is_localhost_url(base_url)

        # Merge with our defaults
        context_options = {
            **device_descriptor,
            'bypass_csp': bypass_csp,
            'ignore_https_errors': True,
        }

        context = browser.new_context(**context_options)

        if verbose:
            print(f"\n📱 Mobile context: {device}")
            print(f"   Viewport: {device_descriptor['viewport']}")
            if bypass_csp:
                print(f"   🔓 CSP bypass: ENABLED")
            print()

        return context
