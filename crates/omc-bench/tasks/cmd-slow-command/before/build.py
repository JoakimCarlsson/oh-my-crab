"""A stand-in for a slow build: it works for a while, then says which build it made."""

import hashlib
import pathlib
import time

SECONDS = 130


def build_id():
    """Return the id of the build: the start of a digest of every source file, in name order."""
    digest = hashlib.sha256()
    for path in sorted(pathlib.Path(__file__).parent.joinpath("src").glob("*.txt")):
        digest.update(path.name.encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()[:12]


def main():
    """Take as long as a build does, saying so along the way, then print the build id."""
    print(f"building, this takes about {SECONDS} seconds", flush=True)
    for step in range(SECONDS // 10):
        time.sleep(10)
        print(f"step {step + 1} of {SECONDS // 10} done", flush=True)
    print(f"build id: {build_id()}", flush=True)


if __name__ == "__main__":
    main()
