#!/usr/bin/env python3
"""Nanoka: switch the default provider to DeepSeek.

- text model:  deepseek-v4-flash
- vision model: deepseek-v4-flash-vision-exp (experimental image input)
- API key:     reads $env:DEEPSEEK_API_KEY at runtime; keep the env var set

Usage:
  python3 scripts/setup_deepseek.py            # edits ~/.nanoka/config/config.jsonc
  NANOKA_HOME=/path python3 scripts/setup_deepseek.py
"""
import argparse
import json
import os
import re
import sys
import time
from pathlib import Path

DEFAULT_HOME = Path.home() / ".nanoka"
DEEPSEEK = {
    "id": "deepseek",
    "display_name": "DeepSeek",
    "base_url": "https://api.deepseek.com",
    "api_key": "$env:DEEPSEEK_API_KEY",
    "models": ["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"],
    "default_model": "deepseek-v4-flash",
    "model_context_window": {
        "deepseek-v4-flash": 1_000_000,
        "deepseek-v4-flash-vision-exp": 1_000_000,
    },
    "model_modalities": {
        "deepseek-v4-flash": ["text"],
        "deepseek-v4-flash-vision-exp": ["text", "image"],
    },
}


def strip_jsonc(text: str) -> str:
    """Strip // and /* */ comments without touching strings."""
    out = []
    i = 0
    n = len(text)
    in_string = None
    while i < n:
        ch = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if in_string:
            out.append(ch)
            if ch == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if ch == in_string:
                in_string = None
            i += 1
            continue
        if ch in "\"'":
            in_string = ch
            out.append(ch)
            i += 1
            continue
        if ch == "/" and nxt == "/":
            i += 2
            while i < n and text[i] not in "\r\n":
                i += 1
            continue
        if ch == "/" and nxt == "*":
            i += 2
            while i + 1 < n and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
            continue
        out.append(ch)
        i += 1
    cleaned = "".join(out)
    cleaned = re.sub(r",(\s*[}\]])", r"\1", cleaned)
    return cleaned


def load_config(path: Path) -> dict:
    if not path.exists():
        sys.exit(f"config not found: {path}\nrun `nanoka init` first")
    with open(path, encoding="utf-8") as handle:
        return json.loads(strip_jsonc(handle.read()))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", default=os.environ.get("NANOKA_HOME", DEFAULT_HOME), help="NANOKA_HOME root")
    args = parser.parse_args()

    root = Path(args.home).expanduser()
    path = root / "config" / "config.jsonc"
    config = load_config(path)

    config["active_provider"] = "deepseek"
    config["active_provider_models"] = [
        {"provider_id": "deepseek", "model": "deepseek-v4-flash"}
    ]
    config["active_multimodal_provider_models"] = [
        {"provider_id": "deepseek", "model": "deepseek-v4-flash-vision-exp"}
    ]

    providers = config.setdefault("providers", [])
    index = next(
        (i for i, provider in enumerate(providers) if provider.get("id") == "deepseek"),
        None,
    )
    provider = DEEPSEEK
    if index is None:
        providers.append(provider)
    else:
        providers[index] = {**providers[index], **provider}

    backup = path.with_name(f"config.jsonc.bak-{int(time.time())}")
    backup.write_text(path.read_text(encoding="utf-8"), encoding="utf-8")
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(config, handle, ensure_ascii=False, indent=2)
        handle.write("\n")

    print(f"DeepSeek configured in {path}")
    print(f"  text:   deepseek-v4-flash")
    print(f"  vision: deepseek-v4-flash-vision-exp")
    print(f"  key:    $env:DEEPSEEK_API_KEY (make sure the env var is exported)")
    print(f"backup:  {backup}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
