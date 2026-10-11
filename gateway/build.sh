#!/bin/sh
# Build cpxy-tsgw: the Rust packet engine as a static library, then the Go binary linked to it.
#
# usage: build.sh [<target>] [<output>]
#   <target>  native (default): for this machine, dynamically linked to its libc
#             x86_64-unknown-linux-musl, aarch64-unknown-linux-musl: fully static, built with
#             `zig cc` (zig must be on PATH, or set ZIG)
#   <output>  the binary to write, default ../target/<target>/release/cpxy-tsgw
#
# `build.sh test` runs the Go tests against a native engine instead.
set -eu

target="${1:-native}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"

case "$target" in
native | test)
	cargo build --manifest-path "$repo/Cargo.toml" --release --locked -p mobile-engine --lib
	libdir="$repo/target/release"
	# By path, as cargo builds a shared library beside it. The rest is what
	# `cargo rustc -- --print native-static-libs` lists for glibc.
	export CGO_LDFLAGS="$libdir/libmobile_engine.a -lgcc_s -lutil -lrt"
	export CGO_ENABLED=1
	cd "$here"
	if [ "$target" = test ]; then
		exec go test ./...
	fi
	out="${2:-$libdir/cpxy-tsgw}"
	go build -trimpath -ldflags '-s -w' -o "$out" ./cmd/cpxy-tsgw
	;;
x86_64-unknown-linux-musl | aarch64-unknown-linux-musl)
	arch="${target%%-*}"
	case "$arch" in
	x86_64) goarch=amd64 ;;
	aarch64) goarch=arm64 ;;
	esac
	zig="${ZIG:-zig}"
	command -v "$zig" >/dev/null || { echo "zig is required for $target" >&2; exit 1; }

	# One compiler for both the C in Rust dependencies (ring, SQLite) and cgo's linking.
	# A fixed place, so cargo's build scripts see the same CC each time and do not rebuild.
	tools="$repo/target/zig-cc/$arch"
	mkdir -p "$tools"
	# cc-rs adds --target=<Rust triple>, which zig does not understand: drop it.
	cat >"$tools/cc" <<CC
#!/bin/sh
for arg; do
	shift
	case "\$arg" in --target=*) ;; *) set -- "\$@" "\$arg" ;; esac
done
exec "$(command -v "$zig")" cc -target $arch-linux-musl "\$@"
CC
	printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$(command -v "$zig")" >"$tools/ar"
	chmod +x "$tools/cc" "$tools/ar"

	triple_env="$(echo "$target" | tr 'a-z-' 'A-Z_')"
	cc_var="CC_$(echo "$target" | tr '-' '_')"
	ar_var="AR_$(echo "$target" | tr '-' '_')"
	rustup target add "$target" >/dev/null 2>&1 || true
	env "$cc_var=$tools/cc" "$ar_var=$tools/ar" "CARGO_TARGET_${triple_env}_LINKER=$tools/cc" \
		cargo build --manifest-path "$repo/Cargo.toml" --release --locked --target "$target" -p mobile-engine --lib
	libdir="$repo/target/$target/release"

	# Rust's musl targets unwind with their own libunwind, which zig does not link by default.
	unwind="$(cd "$repo" && rustc --print sysroot)/lib/rustlib/$target/lib/self-contained/libunwind.a"

	out="${2:-$libdir/cpxy-tsgw}"
	cd "$here"
	CC="$tools/cc" CGO_ENABLED=1 GOOS=linux GOARCH="$goarch" CGO_LDFLAGS="$libdir/libmobile_engine.a $unwind" \
		go build -trimpath -tags netgo,osusergo \
		-ldflags '-s -w -linkmode external -extldflags "-static"' -o "$out" ./cmd/cpxy-tsgw
	;;
*)
	sed -n '2,11p' "$0" >&2
	exit 2
	;;
esac
echo "$out"
