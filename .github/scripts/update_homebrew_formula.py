#!/usr/bin/env python3

from __future__ import annotations

import argparse
from pathlib import Path
import re


def class_name(binary_name: str) -> str:
    pieces = re.split(r"[-_]+", binary_name.strip())
    return "".join(piece.capitalize() for piece in pieces if piece)


def render_formula(
    *,
    binary: str,
    description: str,
    repo: str,
    tag: str,
    version: str,
    sha_intel: str,
    sha_arm: str,
) -> str:
    cls = class_name(binary)
    url_prefix = f"https://github.com/{repo}/releases/download/{tag}"

    return (
        f"class {cls} < Formula\n"
        f'  desc "{description}"\n'
        f'  homepage "https://github.com/{repo}"\n'
        '  license "MIT"\n'
        f'  version "{version}"\n'
        "\n"
        "  on_macos do\n"
        "    on_intel do\n"
        f'      url "{url_prefix}/{binary}-x86_64-apple-darwin.tar.gz"\n'
        f'      sha256 "{sha_intel}"\n'
        "    end\n"
        "    on_arm do\n"
        f'      url "{url_prefix}/{binary}-aarch64-apple-darwin.tar.gz"\n'
        f'      sha256 "{sha_arm}"\n'
        "    end\n"
        "  end\n"
        "\n"
        "  def install\n"
        f'    bin.install "{binary}"\n'
        "  end\n"
        "\n"
        "  def caveats\n"
        "    <<~EOS\n"
        f"      {binary} requires the GitHub CLI (gh) to be installed and authenticated:\n"
        "        brew install gh\n"
        "        gh auth login\n"
        "\n"
        "      Optional: Install delta for improved diff visualization:\n"
        "        brew install git-delta\n"
        "\n"
        "      Optional: For AI-powered reviews, install one of:\n"
        "        npm install -g @anthropic-ai/claude-code\n"
        "        npm install -g @openai/codex\n"
        "\n"
        "      Optional: Install the code-review skill for enhanced AI reviews:\n"
        "        mkdir -p ~/.claude/skills/code-review\n"
        "        # Copy SKILL.md from the repository to ~/.claude/skills/code-review/\n"
        "        # Or for Codex: ~/.codex/skills/code-review/\n"
        "\n"
        "      Optional: Create a custom review guide:\n"
        "        mkdir -p ~/.config/reviewer\n"
        "        # Add your review preferences to ~/.config/reviewer/review_guide.md\n"
        "    EOS\n"
        "  end\n"
        "\n"
        "  test do\n"
        f'    assert_match version.to_s, shell_output("#{{bin}}/{binary} --version")\n'
        "  end\n"
        "end\n"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render/update a Homebrew formula for release artifacts."
    )
    parser.add_argument("--formula", required=True, help="Path to Formula/<name>.rb")
    parser.add_argument("--repo", required=True, help="GitHub repository owner/name")
    parser.add_argument("--tag", required=True, help="Release tag (example: v0.5.1)")
    parser.add_argument("--version", required=True, help="Release version (example: 0.5.1)")
    parser.add_argument("--binary", required=True, help="Binary and formula base name")
    parser.add_argument("--sha-intel", required=True, help="SHA256 for x86_64 macOS tarball")
    parser.add_argument("--sha-arm", required=True, help="SHA256 for aarch64 macOS tarball")
    parser.add_argument(
        "--desc",
        default="TUI for reviewing GitHub PRs with Claude Code/Codex integration",
        help="Formula description",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    formula_path = Path(args.formula)
    formula_path.parent.mkdir(parents=True, exist_ok=True)

    rendered = render_formula(
        binary=args.binary,
        description=args.desc,
        repo=args.repo,
        tag=args.tag,
        version=args.version,
        sha_intel=args.sha_intel,
        sha_arm=args.sha_arm,
    )
    formula_path.write_text(rendered, encoding="utf-8")


if __name__ == "__main__":
    main()
