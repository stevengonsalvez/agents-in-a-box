"""Setup for Global Learnings CLI."""

from setuptools import setup

setup(
    name="learnings-cli",
    version="2.0.0",
    description="CLI for Global Learnings knowledge base with GraphRAG",
    author="Claude Code Toolkit",
    py_modules=["learnings_cli", "graph_engine", "entity_store", "graspologic_shim"],
    # NOTE: nano-graphrag must be installed separately with --no-deps
    # due to broken transitive deps (graspologic -> numba -> llvmlite).
    # The bash wrapper handles this automatically.
    install_requires=[
        "sentence-transformers>=2.2.0",
        "click>=8.0",
        "rich>=13.0",
        "pyyaml>=6.0",
        "numpy>=1.24.0",
        "networkx>=3.0",
        "tiktoken>=0.5.0",
        "nano-vectordb",
        "openai>=1.0",
        "tenacity>=8.0",
        "hnswlib>=0.7.0",
        "xxhash>=3.0",
        "neo4j>=5.0",
        "pyvis>=0.3.0",
    ],
    entry_points={
        "console_scripts": [
            "learnings=learnings_cli:cli",
        ],
    },
    python_requires=">=3.9",
)
