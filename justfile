# Worktree-level just dispatch. Domain-specific recipes live in
# sibling `.just` modules so other surfaces (hangar, swarm, …) can
# slot in later without recipe-name collisions.
#
# Usage:
#   just --list-submodules           # see every domain
#   just skill-manager --list        # see skill-manager's recipes
#   just skill-manager up --tier full
#
# Install: `brew install just` (or your platform's equivalent).

set shell := ["bash", "-cu"]

mod skill-manager 'skill-manager.just'

# Default — list everything (top-level + submodules).
default:
    @just --list
