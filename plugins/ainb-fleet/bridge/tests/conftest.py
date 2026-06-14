"""Make the bridge package importable without an install step."""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRIDGE_DIR = HERE.parent  # plugins/ainb-fleet/bridge
sys.path.insert(0, str(BRIDGE_DIR))
