"""Setup for Global Learnings CLI."""

from setuptools import setup, find_packages

setup(
    name="learnings-cli",
    version="1.0.0",
    description="CLI for Global Learnings knowledge base with GraphRAG",
    author="Claude Code Toolkit",
    py_modules=["learnings_cli"],
    install_requires=[
        "nano-graphrag>=0.1.0",
        "sentence-transformers>=2.2.0",
        "click>=8.0",
        "rich>=13.0",
        "pyyaml>=6.0",
        "numpy>=1.24.0",
        "pandas>=2.0.0",
        "pyarrow>=14.0.0",
    ],
    entry_points={
        "console_scripts": [
            "learnings=learnings_cli:cli",
        ],
    },
    python_requires=">=3.9",
)
