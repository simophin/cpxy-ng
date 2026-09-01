#!/usr/bin/env bash

set -euo pipefail

readonly project_dir="client/android-app/desktopApp"
readonly binaries_dir="$project_dir/build/compose/binaries/main"
readonly image_dir="$binaries_dir/app"
readonly platform="${1:?usage: verify-desktop-package.sh <linux-x64|macos-x64|macos-arm64>}"

fail() {
  echo "Desktop package verification failed: $*" >&2
  exit 1
}

find_single() {
  local root="$1"
  local name="$2"
  local -a matches=()

  while IFS= read -r match; do
    matches+=("$match")
  done < <(find "$root" -type f -name "$name" -print)

  [[ ${#matches[@]} -eq 1 ]] ||
    fail "expected one $name below $root, found ${#matches[@]}: ${matches[*]:-none}"
  printf '%s\n' "${matches[0]}"
}

verify_macos_binary() {
  local binary="$1"
  local expected_arch="$2"
  local actual_arches
  local dependencies
  local dependency_lines

  actual_arches="$(lipo -archs "$binary")"
  [[ "$actual_arches" == "$expected_arch" ]] ||
    fail "$binary has architecture '$actual_arches', expected '$expected_arch'"

  dependencies="$(otool -L "$binary")"
  printf '%s\n' "$dependencies"
  dependency_lines="$(printf '%s\n' "$dependencies" | tail -n +2)"
  if grep -Evq \
    '^[[:space:]]+(/usr/lib/|/System/Library/|@rpath/|@loader_path/|@executable_path/)' \
    <<<"$dependency_lines"; then
    fail "$binary has a dependency outside the macOS system or application bundle"
  fi
}

case "$platform" in
  linux-x64)
    native_library="$(find_single "$image_dir" libclient.so)"
    readelf_header="$(readelf -h "$native_library")"
    printf '%s\n' "$readelf_header"
    grep -Eq 'Class:\s+ELF64' <<<"$readelf_header" || fail "$native_library is not ELF64"
    grep -Eq 'Machine:\s+Advanced Micro Devices X86-64' <<<"$readelf_header" ||
      fail "$native_library is not x86-64"

    linkage="$(ldd "$native_library")"
    printf '%s\n' "$linkage"
    ! grep -q 'not found' <<<"$linkage" || fail "$native_library has unresolved dependencies"

    portable_archive="$(find_single "$binaries_dir/portable" 'Cpxy-*-linux-x64.tar.gz')"
    mapfile -t archived_libraries < <(tar -tzf "$portable_archive" | grep '/libclient\.so$' || true)
    [[ ${#archived_libraries[@]} -eq 1 ]] ||
      fail "expected one libclient.so in $portable_archive, found ${#archived_libraries[@]}"
    archive_listing="$(tar -tvzf "$portable_archive")"
    grep -Eq '^-rwxr-xr-x .*Cpxy/bin/Cpxy$' <<<"$archive_listing" ||
      fail "$portable_archive does not preserve launcher execute permissions"
    ;;
  macos-x64|macos-arm64)
    expected_arch="x86_64"
    [[ "$platform" == macos-arm64 ]] && expected_arch="arm64"

    image_library="$(find_single "$image_dir" libclient.dylib)"
    verify_macos_binary "$image_library" "$expected_arch"

    dmg="$(find_single "$binaries_dir/dmg" 'Cpxy-*.dmg')"
    mount_dir="$(mktemp -d)"
    cleanup() {
      hdiutil detach "$mount_dir" -quiet || true
      rmdir "$mount_dir" || true
    }
    trap cleanup EXIT
    hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$mount_dir" -quiet
    dmg_library="$(find_single "$mount_dir" libclient.dylib)"
    verify_macos_binary "$dmg_library" "$expected_arch"
    ;;
  *)
    fail "unsupported platform '$platform'"
    ;;
esac

echo "Verified Desktop package for $platform"
