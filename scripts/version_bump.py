#!/usr/bin/env python3
import json
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

PACKAGE_JSON = REPO_ROOT / "package.json"
PACKAGE_LOCK = REPO_ROOT / "package-lock.json"
TAURI_CONF = REPO_ROOT / "src-tauri" / "tauri.conf.json"
CARGO_TOML = REPO_ROOT / "src-tauri" / "Cargo.toml"
RELEASE_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"

def get_current_version() -> str:
    data = json.loads(PACKAGE_JSON.read_text())
    version = data.get("version")
    if not isinstance(version, str) or not version.strip():
        raise SystemExit(f"Could not determine current version from {PACKAGE_JSON}")
    return version

def prompt(message: str) -> str:
    value = input(message).strip()
    if not value:
        raise SystemExit("Input cannot be empty.")
    return value

def update_json_version(path: Path, version: str) -> None:
    data = json.loads(path.read_text())
    data["version"] = version

    packages = data.get("packages")
    if isinstance(packages, dict) and "" in packages:
        root_package = packages[""]
        if isinstance(root_package, dict):
            root_package["version"] = version

    path.write_text(json.dumps(data, indent=2) + "\n")
    print(f"Updated {path.relative_to(REPO_ROOT)} version to {version}")

def update_tauri_conf(path: Path, version: str) -> None:
    data = json.loads(path.read_text())
    data["version"] = version
    path.write_text(json.dumps(data, indent=2) + "\n")
    print(f"Updated {path.relative_to(REPO_ROOT)} version to {version}")

def update_cargo_version(path: Path, version: str) -> None:
    original = path.read_text().splitlines()
    version_pattern = re.compile(r'^version\s*=\s*"[^"]*"\s*$')
    replaced = False
    new_lines = []

    for line in original:
        if version_pattern.match(line):
            line = f'version = "{version}"'
            replaced = True
        new_lines.append(line)

    if not replaced:
        raise SystemExit(f"No version field found in {path}")

    path.write_text("\n".join(new_lines) + "\n")
    print(f"Updated {path.relative_to(REPO_ROOT)} version to {version}")

def escape_for_yaml(value: str) -> str:
    return json.dumps(value)

def update_release_body(path: Path, body: str) -> None:
    escaped_body = escape_for_yaml(body)
    new_lines = []
    replaced = False

    with path.open() as f:
        for line in f:
            if line.lstrip().startswith("releaseBody:"):
                indent = " " * (len(line) - len(line.lstrip()))
                new_lines.append(f"{indent}releaseBody: {escaped_body}\n")
                replaced = True
            else:
                new_lines.append(line)

    if not replaced:
        raise SystemExit(f"Could not find releaseBody in {path}")

    path.write_text("".join(new_lines))
    print(f"Updated releaseBody in {path.relative_to(REPO_ROOT)}")


def main() -> None:
    print("Local version bump utility\n")
    current_version = get_current_version()
    version = prompt(f"Enter the new version [{current_version}] (e.g. 1.2.3): ")
    release_body = prompt("Enter the release notes/description: ")

    update_json_version(PACKAGE_JSON, version)
    update_json_version(PACKAGE_LOCK, version)
    update_tauri_conf(TAURI_CONF, version)
    update_cargo_version(CARGO_TOML, version)
    update_release_body(RELEASE_WORKFLOW, release_body)

    print("\nAll files updated. Don't forget to review and commit your changes if desired.")


if __name__ == "__main__":
    main()