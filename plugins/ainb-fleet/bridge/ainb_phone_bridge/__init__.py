# ABOUTME: ainb Telegram phone bridge — relay messages two-way between Telegram
# and a named ainb session (a conductor when present, any session otherwise).
#
# The package is intentionally split so the pure logic (prefix routing, markdown
# -> HTML, 4096 splitting, secret resolution, target resolution) is import-safe
# and unit-testable WITHOUT aiogram or a live ainb binary installed.

__version__ = "0.1.0"

# Telegram hard limit on a single text message.
TG_MAX_LENGTH = 4096

# Default time (seconds) the bridge waits for a session to finish its turn
# before giving up on capturing a reply.
RESPONSE_TIMEOUT = 300
