#!/bin/sh
# Installs staalc from a GitHub release, with no Rust:
#
#   curl -fsSL https://raw.githubusercontent.com/laurells/staal/main/install.sh | sh
#
# It picks the archive for this machine (Linux on x86_64 or arm64, macOS on Apple
# Silicon or Intel), downloads it and its SHA-256, refuses an archive whose hash differs, unpacks
# it into ~/.staal/<version>, and links staalc into ~/.local/bin. Nothing runs as root
# and nothing outside those two folders is touched.
#
# The SHA-256 comes from the same release as the archive, so it shows the download is
# whole, not who built it. Releases after 0.26.0 also carry a build provenance
# attestation, signed by GitHub for the release workflow that built the archive from
# its tag. When the GitHub CLI (gh) is installed and signed in, it is checked, and an
# archive that fails the check is refused. Without gh the archive is checked by its
# SHA-256 only, and the script says so.
#
# Settings, all optional:
#   STAAL_VERSION=0.23.0       a version (the latest release otherwise)
#   STAAL_BIN_DIR=~/bin        where to link staalc (~/.local/bin otherwise)
#   STAAL_HOME=~/.staal        where releases are unpacked
#   STAAL_DOWNLOAD_BASE=...    another place holding the releases' files, laid out as
#                              <base>/v<version>/<archive> (for mirrors and tests)
#   STAAL_REQUIRE_PROVENANCE=1 refuse to install unless the provenance is checked
#   STAAL_GH=gh                the GitHub CLI to check it with
set -eu

repo="laurells/staal"
# The last release made before releases carried a provenance attestation.
last_unattested="0.26.0"
home="${STAAL_HOME:-$HOME/.staal}"
bin_dir="${STAAL_BIN_DIR:-$HOME/.local/bin}"

fail() {
    echo "staal install: $*" >&2
    exit 1
}

case "$(uname -s) $(uname -m)" in
    "Linux x86_64") target="x86_64-unknown-linux-gnu" ;;
    "Linux aarch64" | "Linux arm64") target="aarch64-unknown-linux-gnu" ;;
    "Darwin arm64") target="aarch64-apple-darwin" ;;
    "Darwin x86_64") target="x86_64-apple-darwin" ;;
    *) fail "no release for $(uname -s) on $(uname -m) yet; build from source instead (README.md, \"From source\")" ;;
esac

command -v curl >/dev/null || fail "needs curl"
command -v tar >/dev/null || fail "needs tar"
if command -v sha256sum >/dev/null; then
    sha() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null; then
    sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    fail "needs sha256sum or shasum to check the download"
fi

version="${STAAL_VERSION:-}"
if [ -z "$version" ]; then
    # The latest release's tag, from GitHub's own redirect.
    latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") || fail "cannot reach GitHub to find the latest release"
    version="${latest##*/v}"
    case "$version" in
        [0-9]*) ;;
        *) fail "no release found at https://github.com/$repo/releases" ;;
    esac
fi
case "$version" in
    *[!0-9.]* | "") fail "'$version' is not a version" ;;
esac

base="${STAAL_DOWNLOAD_BASE:-https://github.com/$repo/releases/download}"
name="staalc-$version-$target"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "downloading staalc $version for $target"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/v$version/$name.tar.gz" || fail "no $name.tar.gz in release v$version"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/v$version/$name.tar.gz.sha256" || fail "no checksum for $name.tar.gz in release v$version"
want=$(cut -d' ' -f1 "$tmp/$name.tar.gz.sha256")
have=$(sha "$tmp/$name.tar.gz")
[ "$want" = "$have" ] || fail "the download's SHA-256 is $have, not the release's $want; nothing was installed"

# Whether version $1 comes after version $2 (both digits and dots, up to three parts).
newer() {
    a1=$(echo "$1" | cut -d. -f1); a2=$(echo "$1." | cut -d. -f2); a3=$(echo "$1.." | cut -d. -f3)
    b1=$(echo "$2" | cut -d. -f1); b2=$(echo "$2." | cut -d. -f2); b3=$(echo "$2.." | cut -d. -f3)
    [ "${a1:-0}" -ne "${b1:-0}" ] && { [ "${a1:-0}" -gt "${b1:-0}" ]; return; }
    [ "${a2:-0}" -ne "${b2:-0}" ] && { [ "${a2:-0}" -gt "${b2:-0}" ]; return; }
    [ "${a3:-0}" -gt "${b3:-0}" ]
}

# The build provenance: made by this repository's release workflow, from the tag
# v<version>, on a GitHub-hosted runner.
gh="${STAAL_GH:-gh}"
if command -v "$gh" >/dev/null 2>&1 && "$gh" auth status >/dev/null 2>&1; then
    if "$gh" attestation verify "$tmp/$name.tar.gz" --repo "$repo" \
        --signer-workflow "$repo/.github/workflows/release.yml" \
        --source-ref "refs/tags/v$version" --deny-self-hosted-runners >"$tmp/verify.log" 2>&1; then
        echo "build provenance verified: built by $repo's release workflow from tag v$version"
    elif newer "$version" "$last_unattested"; then
        cat "$tmp/verify.log" >&2
        fail "the archive's build provenance does not verify (gh attestation verify, above); nothing was installed"
    elif [ "${STAAL_REQUIRE_PROVENANCE:-}" = 1 ]; then
        fail "v$version was released before releases carried a build provenance attestation, and STAAL_REQUIRE_PROVENANCE=1; nothing was installed"
    else
        echo "note: v$version was released before releases carried a build provenance attestation; checked by its SHA-256 only"
    fi
elif [ "${STAAL_REQUIRE_PROVENANCE:-}" = 1 ]; then
    fail "STAAL_REQUIRE_PROVENANCE=1, but the build provenance cannot be checked: it needs the GitHub CLI ($gh), installed and signed in (gh auth login); nothing was installed"
else
    echo "note: build provenance not checked (it needs the GitHub CLI, installed and signed in); checked by its SHA-256 only"
fi

# Unpacked beside, then moved into place, so a failure leaves no half install.
mkdir -p "$home" "$bin_dir"
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
[ -x "$tmp/$name/staalc" ] || fail "the archive holds no staalc"
rm -rf "$home/$version"
mv "$tmp/$name" "$home/$version"
ln -sf "$home/$version/staalc" "$bin_dir/staalc"

echo "installed: $("$bin_dir/staalc" --version) at $bin_dir/staalc"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "add $bin_dir to your PATH to run staalc from anywhere, e.g.: echo 'export PATH=\"$bin_dir:\$PATH\"' >> ~/.profile" ;;
esac
