GO ?= go
GOFLAGS ?= -mod=readonly
BIN := bin/server

.PHONY: build test lint clean

build:
	mkdir -p bin
	$(GO) build $(GOFLAGS) -o $(BIN) ./cmd/server

test:
	$(GO) test $(GOFLAGS) ./...

lint:
	gofmt -l .
	$(GO) vet ./...

clean:
	rm -rf bin
