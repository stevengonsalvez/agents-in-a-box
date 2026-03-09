"""Root conftest: wire up source directories onto sys.path."""

import sys
from pathlib import Path

TOOLKIT_ROOT = Path(__file__).resolve().parent.parent
SKILLS_ROOT = TOOLKIT_ROOT / "packages" / "skills"

# Add script directories so tests can import source modules directly
for scripts_dir in [
    SKILLS_ROOT / "global-learnings" / "scripts",
    SKILLS_ROOT / "reflect" / "scripts",
    SKILLS_ROOT / "reflect" / "hooks",
]:
    dir_str = str(scripts_dir)
    if dir_str not in sys.path:
        sys.path.insert(0, dir_str)

# Expose useful paths as module-level constants
SKILLS_DIR = SKILLS_ROOT
RESEARCH_SCRIPTS = SKILLS_ROOT / "research" / "scripts"
GLOBAL_LEARNINGS_SCRIPTS = SKILLS_ROOT / "global-learnings" / "scripts"
REFLECT_SCRIPTS = SKILLS_ROOT / "reflect" / "scripts"
