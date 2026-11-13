run:
	cargo build --bin kandrium-app && ./target/debug/kandrium-app

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets

fmt:
	cargo fmt
