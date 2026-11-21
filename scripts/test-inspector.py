"""Run every native integration case in bounded batches, or one reproducible CI shard."""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shard", type=int, default=0, help="one-based shard number; zero runs all")
    parser.add_argument("--shards", type=int, default=12)
    parser.add_argument("--test-threads", type=int, default=3)
    args = parser.parse_args()
    if args.shards < 1 or not 0 <= args.shard <= args.shards or not 1 <= args.test_threads <= 3:
        parser.error("expected 1..3 test threads and a valid shard number")
    root = Path(__file__).resolve().parents[1]
    cargo = ["cargo", "test", "--release", "--offline", "--locked", "-j", "4",
             "--workspace", "--test", "inspector", "--no-run", "--message-format=json"]
    built = subprocess.check_output(cargo, cwd=root, text=True)
    binaries = [r["executable"] for r in map(json.loads, built.splitlines())
                if r.get("reason") == "compiler-artifact" and r["target"]["name"] == "inspector"
                and r.get("executable") and r["profile"]["test"]]
    if len(binaries) != 1:
        raise RuntimeError(f"expected one Cargo integration-test executable, found {binaries}")
    binary = binaries[0]
    listing = subprocess.check_output([binary, "--list", "--format", "terse"], cwd=root, text=True)
    names = sorted(line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test"))
    if not names or len(names) != len(set(names)):
        raise RuntimeError("empty or ambiguous inspector test list")
    env = os.environ.copy()
    cli = Path(binary).resolve().parent.parent / ("board.exe" if os.name == "nt" else "board")
    if not cli.is_file():
        parser.error("build the locked release workspace binaries first")
    env["BOARD_UI_TEST_CLI"] = str(cli)
    groups = [names[i::args.shards] for i in range(args.shards)]
    selected = groups if args.shard == 0 else [groups[args.shard - 1]]
    for group in selected:
        if not group:
            continue
        print(f"Running {len(group)} of {len(names)} discovered native cases", flush=True)
        command = [binary, "--include-ignored", "--test-threads", str(args.test_threads),
                   "--exact", *group]
        result = subprocess.run(command, cwd=root, env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True)
        print(result.stdout, flush=True)
        result.check_returncode()
        if f"test result: ok. {len(group)} passed; 0 failed; 0 ignored;" not in result.stdout:
            raise RuntimeError("test runner did not account for every selected case")


if __name__ == "__main__":
    main()
