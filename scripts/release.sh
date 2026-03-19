#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# All crate Cargo.toml files that carry the workspace version
CARGO_FILES=(
    crates/zremote-protocol/Cargo.toml
    crates/zremote-core/Cargo.toml
    crates/zremote-agent/Cargo.toml
    crates/zremote-server/Cargo.toml
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

current_version() {
    # Read version from the first crate (all are kept in sync)
    sed -n 's/^version = "\(.*\)"/\1/p' "${CARGO_FILES[0]}"
}

validate_semver() {
    local v="$1"
    if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
        echo "error: '$v' is not a valid semver version (expected X.Y.Z[-prerelease])" >&2
        return 1
    fi
}

check_versions_in_sync() {
    local expected
    expected="$(current_version)"
    for f in "${CARGO_FILES[@]}"; do
        local v
        v="$(sed -n 's/^version = "\(.*\)"/\1/p' "$f")"
        if [[ "$v" != "$expected" ]]; then
            echo "error: version mismatch - ${CARGO_FILES[0]} has $expected but $f has $v" >&2
            return 1
        fi
    done
}

tag_exists_local() {
    git tag -l "v$1" | grep -q "v$1"
}

tag_exists_remote() {
    git ls-remote --tags origin "refs/tags/v$1" 2>/dev/null | grep -q "v$1"
}

bump_part() {
    local cur="$1" part="$2"
    local major minor patch
    IFS='.' read -r major minor patch <<< "${cur%%-*}"
    case "$part" in
        major) echo "$((major + 1)).0.0" ;;
        minor) echo "${major}.$((minor + 1)).0" ;;
        patch) echo "${major}.${minor}.$((patch + 1))" ;;
        *) echo "error: unknown bump type '$part'" >&2; return 1 ;;
    esac
}

set_version() {
    local new_ver="$1"
    local old_ver
    old_ver="$(current_version)"
    for f in "${CARGO_FILES[@]}"; do
        sed -i "s/^version = \"${old_ver}\"/version = \"${new_ver}\"/" "$f"
    done
}

ensure_clean_tree() {
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "error: working tree is dirty, commit or stash changes first" >&2
        return 1
    fi
}

ensure_on_main() {
    local branch
    branch="$(git branch --show-current)"
    if [[ "$branch" != "main" ]]; then
        echo "error: releases must be done from 'main' branch (currently on '$branch')" >&2
        return 1
    fi
}

# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

cmd_next() {
    local cur
    cur="$(current_version)"
    echo "Current version: $cur"
    echo ""
    echo "Next versions:"
    echo "  patch  $(bump_part "$cur" patch)"
    echo "  minor  $(bump_part "$cur" minor)"
    echo "  major  $(bump_part "$cur" major)"
}

cmd_options() {
    local cur
    cur="$(current_version)"
    printf '%s\t%s\n' "patch" "Patch ($(bump_part "$cur" patch))"
    printf '%s\t%s\n' "minor" "Minor ($(bump_part "$cur" minor))"
    printf '%s\t%s\n' "major" "Major ($(bump_part "$cur" major))"
}

cmd_release() {
    local version="$1"

    # Accept shorthand: patch, minor, major
    case "$version" in
        patch|minor|major)
            version="$(bump_part "$(current_version)" "$version")"
            echo "Resolved bump to: $version"
            ;;
    esac

    validate_semver "$version"
    ensure_clean_tree
    ensure_on_main
    check_versions_in_sync

    local cur
    cur="$(current_version)"
    if [[ "$version" == "$cur" ]]; then
        echo "error: version $version is already the current version" >&2
        exit 1
    fi

    if tag_exists_local "$version" || tag_exists_remote "$version"; then
        echo "error: tag v$version already exists" >&2
        exit 1
    fi

    echo "Releasing: $cur -> $version"
    echo ""

    # Bump versions in all Cargo.toml files
    set_version "$version"

    # Regenerate Cargo.lock with updated versions
    cargo check --workspace --quiet

    # Commit, tag, push
    git add "${CARGO_FILES[@]}" Cargo.lock
    git commit -m "Bump version to $version"
    git tag -a "v$version" -m "v$version"
    git push origin main
    git push origin "v$version"

    echo ""
    echo "Released v$version"
    echo "  CI: https://github.com/$(git remote get-url origin | sed 's|.*github.com[:/]\(.*\)\.git|\1|')/actions"
}

cmd_retry() {
    local version="${1:-}"

    if [[ -z "$version" ]]; then
        # Default to latest tag
        version="$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')"
        if [[ -z "$version" ]]; then
            echo "error: no version specified and no tags found" >&2
            exit 1
        fi
        echo "Retrying latest tag: v$version"
    fi

    validate_semver "$version"
    ensure_on_main

    echo "Deleting tag v$version from remote and local..."

    # Delete remote tag
    if tag_exists_remote "$version"; then
        git push origin ":refs/tags/v$version"
        echo "  Deleted remote tag v$version"
    else
        echo "  Remote tag v$version not found (skipping)"
    fi

    # Delete local tag
    if tag_exists_local "$version"; then
        git tag -d "v$version"
        echo "  Deleted local tag v$version"
    else
        echo "  Local tag v$version not found (skipping)"
    fi

    # Verify current version matches
    local cur
    cur="$(current_version)"
    if [[ "$cur" != "$version" ]]; then
        echo "warning: current Cargo.toml version ($cur) differs from tag ($version)" >&2
        echo "  The version commit may be missing. Push first or set version manually." >&2
        exit 1
    fi

    # Re-tag and push
    git tag -a "v$version" -m "v$version"
    git push origin "v$version"

    echo ""
    echo "Re-released v$version"
}

cmd_status() {
    local cur
    cur="$(current_version)"
    echo "Current version: $cur"
    echo ""

    check_versions_in_sync && echo "All Cargo.toml versions in sync" || true
    echo ""

    if tag_exists_local "$cur"; then
        echo "Local tag:  v$cur exists"
    else
        echo "Local tag:  v$cur missing"
    fi

    if tag_exists_remote "$cur"; then
        echo "Remote tag: v$cur exists"
    else
        echo "Remote tag: v$cur missing"
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

usage() {
    cat <<EOF
Usage: $(basename "$0") <command> [args]

Commands:
  next              Show current version and next possible versions
  options           Output version options in tab-separated format (for action inputs)
  release <VER>     Release a new version (VER = X.Y.Z | patch | minor | major)
  retry [VER]       Delete tag and re-push (defaults to latest tag)
  status            Show current version and tag state

Examples:
  $(basename "$0") next
  $(basename "$0") options
  $(basename "$0") release patch
  $(basename "$0") release 0.3.0
  $(basename "$0") retry
  $(basename "$0") retry 0.2.5
EOF
}

case "${1:-}" in
    next)    cmd_next ;;
    options) cmd_options ;;
    release) [[ -z "${2:-}" ]] && { echo "error: version required"; usage; exit 1; }; cmd_release "$2" ;;
    retry)   cmd_retry "${2:-}" ;;
    status)  cmd_status ;;
    *)       usage; exit 1 ;;
esac
