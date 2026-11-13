run:
	cargo build --bin kandrium-app && ./target/debug/kandrium-app

# Rebuild, refresh the dev bundle and launch it outside the agent sandbox.
dev:
	cargo build --bin kandrium-app
	test -d target/Kandrium-dev.app || (echo "missing target/Kandrium-dev.app; see AGENTS.md" && exit 1)
	cp target/debug/kandrium-app target/Kandrium-dev.app/Contents/MacOS/kandrium-app
	open -n target/Kandrium-dev.app

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets

fmt:
	cargo fmt

# Build Kandrium.app + DMG with the root packager config.
bundle:
	cargo packager --config packager.json
