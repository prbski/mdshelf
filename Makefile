CARGO ?= cargo
CONFIG ?= examples/mdshelf.toml

.PHONY: help build release check test clippy fmt fmt-check clean run serve mdshelf-check install

help:
	@echo "Targets:"
	@echo "  make build          - $(CARGO) build"
	@echo "  make release        - $(CARGO) build --release"
	@echo "  make check          - $(CARGO) check"
	@echo "  make test           - $(CARGO) test"
	@echo "  make clippy         - $(CARGO) clippy"
	@echo "  make fmt            - $(CARGO) fmt"
	@echo "  make fmt-check      - $(CARGO) fmt --check"
	@echo "  make clean          - $(CARGO) clean"
	@echo "  make run / serve    - run dev server (CONFIG=$(CONFIG))"
	@echo "  make mdshelf-check  - validate config and scan content"
	@echo "  make install        - $(CARGO) install --path ."
	@echo "Override CONFIG=path/to/mdshelf.toml as needed."

build:
	$(CARGO) build

release:
	$(CARGO) build --release

check:
	$(CARGO) check

test:
	$(CARGO) test --all-features

clippy:
	$(CARGO) clippy --all-targets --all-features

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clean:
	$(CARGO) clean

run: serve

serve:
	$(CARGO) run -- serve --config $(CONFIG)

mdshelf-check:
	$(CARGO) run -- check --config $(CONFIG)

install:
	$(CARGO) install --path .
