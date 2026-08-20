#!/bin/sh
set -eu

umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest="$script_dir/assets.lock.tsv"
destination="$script_dir/out/downloads"
include_optional=0
offline=0
temporary_path=

usage() {
    cat >&2 <<'EOF'
usage: fetch-assets.sh [--all] [--offline] [DESTINATION]

Downloads and verifies the required pinned x86_64 guest assets. --all also
fetches optional media. --offline performs no network access and only verifies
files already present in DESTINATION.
EOF
}

die() {
    printf 'fetch-assets: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary_path" ]; then
        rm -f -- "$temporary_path"
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

verify_file() {
    file_path=$1
    expected_size=$2
    expected_sha=$3

    [ -f "$file_path" ] || return 1
    actual_size=$(wc -c <"$file_path" | tr -d '[:space:]')
    [ "$actual_size" = "$expected_size" ] || return 1
    actual_sha=$(sha256_file "$file_path")
    [ "$actual_sha" = "$expected_sha" ]
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --all)
            include_optional=1
            ;;
        --offline)
            offline=1
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        --*)
            usage
            die "unknown option: $1"
            ;;
        *)
            [ "$destination" = "$script_dir/out/downloads" ] ||
                die "only one destination may be supplied"
            destination=$1
            ;;
    esac
    shift
done

[ -f "$manifest" ] || die "missing lock file: $manifest"
mkdir -p -- "$destination"
destination=$(CDPATH= cd -- "$destination" && pwd)
trap cleanup EXIT HUP INT TERM

tab=$(printf '\t')
while IFS="$tab" read -r asset_id requirement filename expected_size expected_sha url extra; do
    case "$asset_id" in
        ''|'#'*) continue ;;
    esac

    [ -z "${extra:-}" ] || die "unexpected extra field for $asset_id"
    case "$requirement" in
        required) ;;
        optional)
            [ "$include_optional" -eq 1 ] || continue
            ;;
        *) die "invalid requirement for $asset_id: $requirement" ;;
    esac
    case "$filename" in
        ''|*/*|.|..) die "unsafe filename for $asset_id: $filename" ;;
    esac
    case "$expected_size" in
        ''|*[!0-9]*) die "invalid byte size for $asset_id" ;;
    esac
    [ "${#expected_sha}" -eq 64 ] ||
        die "invalid SHA-256 length for $asset_id"
    case "$expected_sha" in
        *[!0-9a-f]*) die "invalid SHA-256 for $asset_id" ;;
    esac
    case "$url" in
        https://*) ;;
        *) die "non-HTTPS URL for $asset_id" ;;
    esac

    target="$destination/$filename"
    if [ -e "$target" ]; then
        verify_file "$target" "$expected_size" "$expected_sha" ||
            die "existing file failed verification: $target"
        printf 'verified  %s\n' "$target"
        continue
    fi

    [ "$offline" -eq 0 ] ||
        die "offline asset is missing: $target"
    command -v curl >/dev/null 2>&1 || die "curl is required for downloads"

    temporary_path=$(mktemp "$destination/.${filename}.tmp.XXXXXX")
    printf 'fetching   %s\n' "$url"
    curl \
        --fail \
        --location \
        --silent \
        --show-error \
        --proto '=https' \
        --proto-redir '=https' \
        --max-redirs 5 \
        --connect-timeout 15 \
        --max-time 900 \
        --max-filesize "$expected_size" \
        --output "$temporary_path" \
        "$url" ||
        die "download failed for $asset_id"

    verify_file "$temporary_path" "$expected_size" "$expected_sha" ||
        die "downloaded file failed verification for $asset_id"
    mv -- "$temporary_path" "$target"
    temporary_path=
    printf 'verified  %s\n' "$target"
done <"$manifest"

