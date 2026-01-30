#!/bin/bash
set -e

# Release script for reviewer
# Usage: ./scripts/release.sh <version>
# Example: ./scripts/release.sh 0.1.0

VERSION=${1:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}

echo "Preparing release v${VERSION}..."

# Ensure we're on main branch and clean
if [[ $(git status --porcelain) ]]; then
    echo "Error: Working directory not clean. Commit or stash changes first."
    exit 1
fi

# Update version in Cargo.toml if needed
CURRENT_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
if [[ "$CURRENT_VERSION" != "$VERSION" ]]; then
    echo "Updating version from $CURRENT_VERSION to $VERSION..."
    sed -i '' "s/^version = \"$CURRENT_VERSION\"/version = \"$VERSION\"/" Cargo.toml
    cargo build --release  # Update Cargo.lock
    git add Cargo.toml Cargo.lock
    git commit -m "Bump version to $VERSION"
fi

# Build release
echo "Building release..."
cargo build --release

# Create git tag
echo "Creating tag v${VERSION}..."
git tag -a "v${VERSION}" -m "Release v${VERSION}"

# Push tag
echo "Pushing tag..."
git push origin main
git push origin "v${VERSION}"

echo ""
echo "Release v${VERSION} tagged and pushed!"
echo ""
echo "Next steps:"
echo "1. Go to https://github.com/daulet/reviewer/releases/new"
echo "2. Select tag v${VERSION}"
echo "3. Generate release notes"
echo "4. Publish release"
echo ""
echo "5. After GitHub release is created, update the formula:"
echo "   - Download the source tarball:"
echo "     curl -sL https://github.com/daulet/reviewer/archive/refs/tags/v${VERSION}.tar.gz -o /tmp/reviewer-${VERSION}.tar.gz"
echo "   - Get SHA256:"
echo "     shasum -a 256 /tmp/reviewer-${VERSION}.tar.gz"
echo "   - Update homebrew/reviewer.rb with the SHA256"
echo ""
echo "6. Copy the formula to your homebrew tap:"
echo "   cp homebrew/reviewer.rb ~/dev/homebrew-tap/Formula/"
