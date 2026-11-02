.PHONY: install fmt lint

install:
	cargo install --path crates/omc --locked

fmt:
	cargo fmt --all

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
