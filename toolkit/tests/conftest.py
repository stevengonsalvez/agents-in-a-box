"""Root conftest: wire up source directories onto sys.path."""

import sys
from pathlib import Path

TOOLKIT_ROOT = Path(__file__).resolve().parent.parent
SKILLS_ROOT = TOOLKIT_ROOT / "packages" / "skills"
PLUGINS_ROOT = TOOLKIT_ROOT / "packages" / "plugins"

# Add script directories so tests can import source modules directly
for scripts_dir in [
    PLUGINS_ROOT / "reflect" / "scripts",
    PLUGINS_ROOT / "reflect" / "hooks",
]:
    dir_str = str(scripts_dir)
    if dir_str not in sys.path:
        sys.path.insert(0, dir_str)

# Expose useful paths as module-level constants
SKILLS_DIR = SKILLS_ROOT
RESEARCH_SCRIPTS = SKILLS_ROOT / "research" / "scripts"
REFLECT_SCRIPTS = PLUGINS_ROOT / "reflect" / "scripts"
