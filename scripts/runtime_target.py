"""Native static target for the disposable Linux runtime acceptance scripts."""
import os
from pathlib import Path


def runtime_target():
    host = os.uname()
    if host.sysname != "Linux" or host.machine not in ("x86_64", "aarch64"):
        raise RuntimeError("runtime acceptance requires native Linux x86_64 or aarch64")
    return Path("target") / f"{host.machine}-unknown-linux-musl" / "debug"
