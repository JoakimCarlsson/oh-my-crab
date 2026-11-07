PREFIX ?= $(HOME)/.local

.PHONY: install fmt lint catalog

install:
	cargo install --path crates/omc --locked --force --root $(PREFIX)

fmt:
	cargo fmt --all

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

catalog:
	curl -fsSL https://models.dev/api.json | cargo run -q -p omc-providers --example catalog -- crates/omc-providers/catalog/models.json
