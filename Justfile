
run:
	cargo build --bin kandrium-app && ./target/debug/kandrium-app

# Build the gpui front-end as a real macOS app and DMG. The crate-local
# `crates/app-gpui/packager.json` drives it; cargo-packager runs with that
# file's directory as the working directory, so every path in it is relative
# to `crates/app-gpui`. Outputs land in `crates/app-gpui/target/release/bundle`.
gpui-bundle:
	cargo build --release --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui
	cargo packager --config crates/app-gpui/packager.json
