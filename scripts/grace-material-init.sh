#!/bin/sh
set -eu

payload=/payload/aarch64
manifest=/payload/manifest.tsv
destination=/forge-boot-artifacts/blobs/internal/aarch64

fail() {
    echo "Grace material initialization failed: $*" >&2
    exit 1
}

validate() {
    root=$1
    tab=$(printf '\t')
    while IFS="$tab" read -r expected size name; do
        case "$name" in
            BUILD-INFO.json|ROOTFS-PACKAGES.json|golan.efi|ipxe.efi|scout.efi|scout.squashfs) ;;
            *) fail "unexpected material name: $name" ;;
        esac
        path=$root/$name
        test -f "$path" && test ! -L "$path" || fail "missing or unsafe material: $name"
        actual_size=$(wc -c < "$path" | tr -d ' ')
        test "$actual_size" = "$size" || fail "unexpected size: $name"
        actual=$(sha256sum "$path")
        actual=${actual%% *}
        test "$actual" = "$expected" || fail "checksum mismatch: $name"
    done < "$manifest"
}

validate "$payload"
parent=${destination%/*}
mkdir -p "$parent"
if test -e "$destination"; then
    test -d "$destination" && test ! -L "$destination" || fail "destination is not a directory"
    validate "$destination"
    echo "GRACE_MATERIAL_READY existing"
    exit 0
fi

temporary=$(mktemp -d "$parent/.aarch64.copy.XXXXXX")
cleanup() {
    test ! -e "$temporary" || rm -rf "$temporary"
}
trap cleanup EXIT INT TERM
tab=$(printf '\t')
while IFS="$tab" read -r _ _ name; do
    cp "$payload/$name" "$temporary/$name"
    chmod 0644 "$temporary/$name"
done < "$manifest"
validate "$temporary"
mv "$temporary" "$destination"
trap - EXIT INT TERM
echo "GRACE_MATERIAL_READY copied"
