"""Expose the shared Ahrara skill to Hermes Agent."""
from pathlib import Path


def register(ctx):
    ctx.register_skill("ahrara", Path(__file__).parent / "skills" / "ahrara" / "SKILL.md")
