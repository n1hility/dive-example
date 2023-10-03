.PHONY: build
build: export RUSTFLAGS=-C target-feature=+crt-static
build:
	cargo build --release --target x86_64-unknown-linux-musl

.PHONY: clean
clean:
	cargo clean

.PHONE: run
run: build
	sudo -k -p "Enter root password to run dive under root:" ./target/x86_64-unknown-linux-musl/release/dive
