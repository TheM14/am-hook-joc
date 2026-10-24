#!/usr/bin/env python3
"""Release tooling for am-hook-joc.

Subcommands
-----------
prepare        download an upstream release source tree, apply the JOC patch and
               install the HRTF fields into it.
package-source  create the full patched source archive from a build tree.
publish        create/update the GitHub release and verify every asset (idempotent).
monitor        one upstream-monitor pass: validate, tag and dispatch releases in
               ascending version order; recover incomplete releases.
--self-test    run the monitor planning tests against a fake GitHub client.

Only the standard library is used; all GitHub access goes through the REST API.
"""
from __future__ import annotations

import argparse
import io
import json
import os
import re
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

UPSTREAM = "itouakirai/am-hook"
WORKFLOW = "release.yml"
SOURCE_TAG = "v0.5.9"  # fallback when the repository has no tags yet
API = "https://api.github.com"


def log(message: str) -> None:
    print(message, flush=True)


def version_key(tag: str) -> tuple[int, ...]:
    match = re.fullmatch(r"v?(\d+(?:\.\d+)*)", tag)
    if not match:
        return ()
    return tuple(int(part) for part in match.group(1).split("."))


def asset_names(tag: str) -> list[str]:
    return [
        f"am-hook-joc-{tag}-windows-x86_64.exe",
        f"am-hook-joc-{tag}-linux-x86_64",
        f"am-hook-joc-{tag}-macos-x86_64",
        f"am-hook-joc-{tag}-macos-aarch64",
        f"am-hook-joc-{tag}.patch",
        f"am-hook-joc-{tag}-source.tar.gz",
        "SHA256SUMS.txt",
    ]


class ApiError(RuntimeError):
    def __init__(self, status: int, message: str):
        super().__init__(f"HTTP {status}: {message}")
        self.status = status


class GitHub:
    def __init__(self, token: str, repo: str):
        self.token = token
        self.repo = repo

    def request(self, method: str, url: str, body: dict | None = None,
                data: bytes | None = None, content_type: str | None = None):
        if not url.startswith("http"):
            url = API + url
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "am-hook-joc-release",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        payload = None
        if body is not None:
            payload = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        elif data is not None:
            payload = data
            headers["Content-Type"] = content_type or "application/octet-stream"
        request = urllib.request.Request(url, data=payload, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return json.loads(response.read() or b"{}")
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")[:500]
            raise ApiError(error.code, detail) from error
        except urllib.error.URLError as error:
            raise RuntimeError(f"cannot reach {url}: {error}") from error

    def release(self, tag: str):
        try:
            return self.request("GET", f"/repos/{self.repo}/releases/tags/{tag}")
        except ApiError as error:
            if error.status == 404:
                return None
            raise

    def releases(self):
        return self.request("GET", f"/repos/{self.repo}/releases?per_page=100")

    def tags(self):
        tags = []
        page = 1
        while True:
            batch = self.request("GET", f"/repos/{self.repo}/tags?per_page=100&page={page}")
            tags.extend(item["name"] for item in batch)
            if len(batch) < 100:
                return tags
            page += 1

    def upstream_releases(self):
        out = []
        page = 1
        while True:
            batch = self.request("GET", f"/repos/{UPSTREAM}/releases?per_page=100&page={page}")
            out.extend(batch)
            if len(batch) < 100:
                return out
            page += 1

    def create_tag(self, tag: str, sha: str) -> None:
        self.request("POST", f"/repos/{self.repo}/git/refs", {
            "ref": f"refs/tags/{tag}", "sha": sha,
        })

    def dispatch(self, tag: str) -> None:
        self.request("POST", f"/repos/{self.repo}/actions/workflows/{WORKFLOW}/dispatches", {
            "ref": tag, "inputs": {"tag": tag},
        })

    def workflow_runs(self):
        return self.request(
            "GET",
            f"/repos/{self.repo}/actions/workflows/{WORKFLOW}/runs"
            "?event=workflow_dispatch&per_page=30",
        )["workflow_runs"]


    def release_assets(self, release: dict) -> list[dict]:
        return self.request("GET", f"/repos/{self.repo}/releases/{release['id']}/assets")


@dataclass
class Plan:
    recover: list[str] = field(default_factory=list)
    publish: list[str] = field(default_factory=list)
    skip: list[str] = field(default_factory=list)


def assets_complete(release: dict | None, tag: str, assets: list[dict] | None) -> bool:
    if not release or release.get("draft"):
        return False
    present = {asset["name"]: asset.get("size", 0) for asset in (assets or [])}
    return all(present.get(name, 0) > 0 for name in asset_names(tag))


def plan(upstream_tags: list[str], local_tags: list[str], releases_by_tag: dict[str, dict | None],
         assets_by_tag: dict[str, list[dict]], patches: set[str]) -> Plan:
    """Pure planning step, used by the monitor and by --self-test."""
    result = Plan()
    local_published = [tag for tag in local_tags if version_key(tag)]
    newest_local = max(local_published, key=version_key) if local_published else None

    # Recovery has priority: an existing tag without a complete release.
    candidates = []
    for tag in sorted(local_tags, key=version_key):
        if not version_key(tag):
            continue
        if not assets_complete(releases_by_tag.get(tag), tag, assets_by_tag.get(tag)):
            candidates.append(tag)
            break
    if candidates:
        result.recover = candidates

    upstream = sorted({tag for tag in upstream_tags if version_key(tag)}, key=version_key)
    if newest_local is None:
        if upstream:
            result.publish = [upstream[-1]]
    else:
        result.publish = [tag for tag in upstream if version_key(tag) > version_key(newest_local)]
    result.publish = [tag for tag in result.publish if tag not in local_tags or tag == (result.recover[0] if result.recover else None)]
    if not result.publish:
        result.skip = [tag for tag in upstream if version_key(tag) <= version_key(newest_local)] if newest_local else []
    return result


def default_branch_sha(client: GitHub, repo: str) -> str:
    branch = client.request("GET", f"/repos/{repo}")["default_branch"]
    return client.request("GET", f"/repos/{repo}/git/ref/heads/{branch}")["object"]["sha"]


def wait_for_release(client: GitHub, tag: str, timeout_s: int = 3600) -> dict | None:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        release = client.release(tag)
        if release and not release.get("draft"):
            assets = client.release_assets(release)
            if assets_complete(release, tag, assets):
                return release
        time.sleep(20)
    return None


def wait_for_run(client: GitHub, tag: str, since: float, timeout_s: int = 3600):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        runs = [run for run in client.workflow_runs()
                if run.get("head_branch") == tag
                and run.get("created_at")
                and _parse_time(run["created_at"]) >= since]
        for run in runs:
            if run.get("status") == "completed":
                return run
        time.sleep(20)
    return None


def _parse_time(value: str) -> float:
    from datetime import datetime
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").timestamp()


# --------------------------------------------------------------------------
# commands


ROOTS = {"src", "Cargo.toml", "Cargo.lock", "build.rs", "tests", ".github", "LICENSE", "README.md"}


def _repo_relative(path: str) -> str:
    """Keep the repository-relative suffix of a patch path."""
    parts = [part for part in path.split("/") if part not in ("", ".", "a", "b")]
    for index, part in enumerate(parts):
        if part in ROOTS:
            return "/".join(parts[index:])
    return "/".join(parts)


def normalize_patch(patch: Path, work: Path) -> Path:
    """Rewrite patch paths so they are relative to the checkout (a/src/...)."""
    source = patch.read_text(encoding="utf-8").splitlines()
    lines = list(source)
    changed = False
    for index, line in enumerate(source):
        is_header = line.startswith("--- ") and index + 1 < len(source) and source[index + 1].startswith("+++ ")
        is_trailer = line.startswith("+++ ") and index > 0 and source[index - 1].startswith("--- ")
        if not (is_header or is_trailer):
            continue
        marker, _, rest = line.partition(" ")
        path, tab, stamp = rest.partition("\t")
        if path == "/dev/null":
            lines[index] = f"{marker} /dev/null"
            continue
        side = "a" if marker == "---" else "b"
        lines[index] = f"{marker} {side}/{_repo_relative(path)}" + (tab + stamp if tab else "")
        changed = True
    if not changed:
        return patch
    target = work / "am-hook-joc.normalized.patch"
    target.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    return target


def command_prepare(args) -> int:
    root = Path(__file__).resolve().parents[1]
    tag = args.tag
    target = Path(args.dir).resolve()
    patch = root / "patches" / f"am-hook-joc-{tag}.patch"
    if not patch.is_file():
        log(f"error: missing patch for {tag}: {patch}")
        return 2
    client = GitHub(args.token, UPSTREAM) if args.token else None
    url = f"https://github.com/{UPSTREAM}/archive/refs/tags/{tag}.tar.gz"
    if client:
        release = client.release(tag) if args.check_release else True
        if release is None:
            log(f"error: upstream has no published release {tag}")
            return 2
    target.parent.mkdir(parents=True, exist_ok=True)
    staging = target.with_name(target.name + "-extract")
    for path in (target, staging):
        if path.exists():
            shutil.rmtree(path)
    staging.mkdir(parents=True)
    log(f"downloading {url}")
    request = urllib.request.Request(url, headers={"User-Agent": "am-hook-joc-release"})
    with urllib.request.urlopen(request, timeout=300) as response:
        payload = response.read()
    with tarfile.open(fileobj=io.BytesIO(payload)) as archive:
        archive.extractall(staging, filter="data")
    entries = [entry for entry in staging.iterdir() if entry.is_dir()]
    if len(entries) != 1:
        log(f"error: unexpected archive layout in {staging}")
        return 2
    source = target
    shutil.move(str(entries[0]), str(source))
    shutil.rmtree(staging)
    import subprocess

    original_patch = patch
    patch = normalize_patch(patch, source.parent)
    # Patches generated from different working trees may still need a different
    # strip level; try the usual ones before giving up.
    result = None
    for strip in (1, 2, 0):
        result = subprocess.run(
            ["git", "apply", f"-p{strip}", "--whitespace=nowarn", str(patch)],
            cwd=source, capture_output=True, text=True)
        if result.returncode == 0:
            log(f"applied patch with -p{strip}")
            break
    else:
        log(f"error: patch does not apply to {tag}:\n{result.stderr}")
        if patch != original_patch:
            patch.unlink(missing_ok=True)
        return 1
    if patch != original_patch:
        patch.unlink(missing_ok=True)
    hrtf = root / "hrtf"
    installed = 0
    for field in sorted(hrtf.glob("*.jochrtf")):
        (source / "src" / "ui" / f"joc-hrtf-{field.stem}.jochrtf").write_bytes(field.read_bytes())
        installed += 1
    log(f"patched {source} with {patch.name}; installed {installed} HRTF fields")
    if args.out:
        Path(args.out).write_text(str(source))
    return 0


def command_package_source(args) -> int:
    root = Path(__file__).resolve().parents[1]
    tag = args.tag
    source = Path(args.build).resolve()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    archive = out / f"am-hook-joc-{tag}-source.tar.gz"
    patch = root / "patches" / f"am-hook-joc-{tag}.patch"
    skipped = {"target", ".git", "node_modules", "dist"}

    def keep(info: tarfile.TarInfo) -> tarfile.TarInfo | None:
        parts = Path(info.name).parts
        return None if any(part in skipped for part in parts) else info

    with tarfile.open(archive, "w:gz") as tar:
        tar.add(source, arcname=f"am-hook-joc-{tag}", filter=keep)
        tar.add(patch, arcname=f"am-hook-joc-{tag}/patches/{patch.name}")
        # The Rust renderer and its build tooling are not part of upstream.
        for name in ("joc-wasm", "tools", "scripts", "patches"):
            tar.add(root / name, arcname=f"am-hook-joc-{tag}/joc-src/{name}", filter=keep)
        for name in ("README.md", "README.zh-CN.md", "LICENSE", "THIRD_PARTY_NOTICES.md"):
            tar.add(root / name, arcname=f"am-hook-joc-{tag}/joc-src/{name}")
    log(f"wrote {archive} ({archive.stat().st_size} bytes)")
    # The archive must carry the applied patch, the wasm renderer and the fields.
    with tarfile.open(archive) as tar:
        names = set(tar.getnames())
    required = [
        f"am-hook-joc-{tag}/src/ui/joc-decode-worker.js",
        f"am-hook-joc-{tag}/src/ui/joc.wasm",
        f"am-hook-joc-{tag}/src/ui/joc-hrtf-D2.jochrtf",
        f"am-hook-joc-{tag}/LICENSE",
        f"am-hook-joc-{tag}/joc-src/joc-wasm/crates/joc-core/src/lib.rs",
        f"am-hook-joc-{tag}/joc-src/LICENSE",
    ]
    missing = [name for name in required if name not in names]
    if missing:
        log(f"error: source archive misses {missing}")
        return 1
    return 0


def command_publish(args) -> int:
    tag = args.tag
    assets_dir = Path(args.assets).resolve()
    client = GitHub(args.token, args.repo)
    names = asset_names(tag)
    missing = [name for name in names if not (assets_dir / name).is_file()
               or (assets_dir / name).stat().st_size == 0]
    if missing:
        log(f"error: missing local assets: {missing}")
        return 1
    if args.dry_run:
        log(f"assets ready for {tag}: {names}")
        return 0
    release = client.release(tag)
    if release is None:
        body = (f"JOC object rendering patch for am-hook {tag}.\n\n"
                f"Built from the upstream {tag} release source with the patch in "
                f"`am-hook-joc-{tag}.patch`; the full patched source is attached.\n")
        release = client.request("POST", f"/repos/{args.repo}/releases", {
            "tag_name": tag, "name": f"am-hook-joc {tag}", "body": body,
            "draft": True, "prerelease": False,
        })
        log(f"created draft release {release['id']}")
    existing = {asset["name"] for asset in client.release_assets(release)}
    upload_url = release["upload_url"].split("{")[0]
    for name in names:
        if name in existing:
            continue
        data = (assets_dir / name).read_bytes()
        log(f"uploading {name} ({len(data)} bytes)")
        client.request("POST", f"{upload_url}?name={name}", data=data,
                       content_type="application/octet-stream")
    assets = []
    for attempt in range(5):
        assets = client.release_assets(release)
        present = {asset["name"]: asset.get("size", 0) for asset in assets}
        if all(present.get(name, 0) > 0 for name in names):
            break
        time.sleep(2)
    else:
        log("error: release assets are incomplete after upload")
        return 1
    if release.get("draft"):
        client.request("PATCH", f"/repos/{args.repo}/releases/{release['id']}", {"draft": False})
        release = client.release(tag)
        assets = client.release_assets(release)
    if not assets_complete(release, tag, assets):
        log("error: release is not publishable")
        return 1
    log(f"published {tag} with {len(assets)} assets")
    return 0


def command_monitor(args) -> int:
    client = GitHub(args.token, args.repo)
    releases = [r for r in client.upstream_releases()
                if not r.get("draft") and not r.get("prerelease")]
    upstream = [r["tag_name"] for r in releases]
    local_tags = client.tags()
    local_releases = {tag: client.release(tag) for tag in local_tags}
    assets = {tag: (client.release_assets(release) if release else [])
              for tag, release in local_releases.items()}
    patches = {path.name[len("am-hook-joc-"):-len(".patch")]
               for path in (Path(__file__).resolve().parents[1] / "patches").glob("am-hook-joc-*.patch")}
    decision = plan(upstream, local_tags, local_releases, assets, patches)
    needed = decision.recover + decision.publish
    missing = [tag for tag in needed if tag not in patches]
    log(f"decision: recover={decision.recover} publish={decision.publish}"
        + (f" missing_patch={missing}" if missing else ""))
    if args.dry_run:
        return 2 if missing else 0
    if missing:
        log(f"error: no patch for {missing[0]}; add patches/am-hook-joc-{missing[0]}.patch")
        return 2
    for tag in decision.recover + decision.publish:
        if tag not in local_tags:
            sha = default_branch_sha(client, args.repo)
            client.create_tag(tag, sha)
            log(f"created tag {tag} at {sha}")
            local_tags.append(tag)
        since = time.time() - 120
        client.dispatch(tag)
        log(f"dispatched {WORKFLOW} for {tag}")
        run = wait_for_run(client, tag, since, args.timeout)
        if run is None:
            log(f"error: {tag} workflow did not finish")
            return 1
        if run.get("conclusion") != "success":
            log(f"error: {tag} workflow concluded {run.get('conclusion')}")
            return 1
        release = wait_for_release(client, tag, args.timeout)
        if release is None:
            log(f"error: {tag} release is incomplete")
            return 1
        log(f"{tag} published with {len(client.release_assets(release))} assets")
    return 0


# --------------------------------------------------------------------------
# self-test (planning only)


class FakeGitHub:
    def __init__(self, upstream: list[str], local: list[str], releases: dict[str, dict | None],
                 assets: dict[str, list[dict]]):
        self.upstream = upstream
        self.local = local
        self.releases = releases
        self.assets = assets

    def upstream_releases(self):
        return [{"tag_name": tag, "draft": False, "prerelease": False} for tag in self.upstream]

    def tags(self):
        return list(self.local)

    def release(self, tag):
        return self.releases.get(tag)

    def release_assets(self, release):
        return self.assets.get(release["tag_name"], [])


def complete_assets(tag: str) -> list[dict]:
    return [{"name": name, "size": 1} for name in asset_names(tag)]


def run_self_test() -> int:
    failures = []

    def check(name, actual, expected):
        if actual != expected:
            failures.append(f"{name}: {actual} != {expected}")

    # First run: no local tags, several upstream releases -> only the latest.
    d = plan(["v0.5.1", "v0.5.7", "v0.5.9"], [], {}, {}, set())
    check("initial", d.publish, ["v0.5.9"])

    # Up to date: no action.
    local = ["v0.5.9"]
    rel = {"v0.5.9": {"tag_name": "v0.5.9", "draft": False}}
    ast = {"v0.5.9": complete_assets("v0.5.9")}
    d = plan(["v0.5.7", "v0.5.9"], local, rel, ast, {"v0.5.9"})
    check("noop", (d.publish, d.recover), ([], []))

    # New versions, ascending, multiple pending.
    d = plan(["v0.5.9", "v0.5.10", "v0.5.12", "v0.5.11"], local, rel, ast, {"v0.5.9"})
    check("ascending", d.publish, ["v0.5.10", "v0.5.11", "v0.5.12"])

    # Incomplete release is recovered first.
    broken = {"v0.5.9": {"tag_name": "v0.5.9", "draft": False}}
    d = plan(["v0.5.9", "v0.5.10"], local, broken, {"v0.5.9": []}, {"v0.5.9"})
    check("recover", (d.recover, d.publish), (["v0.5.9"], ["v0.5.10"]))

    # Draft release counts as missing.
    draft = {"v0.5.9": {"tag_name": "v0.5.9", "draft": True}}
    d = plan(["v0.5.9"], local, draft, {"v0.5.9": complete_assets("v0.5.9")}, {"v0.5.9"})
    check("draft", d.recover, ["v0.5.9"])

    if failures:
        for failure in failures:
            log(f"FAIL {failure}")
        return 1
    log("PASS: monitor planning covers initial, no-op, ascending, recovery and draft cases")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--self-test", action="store_true")
    sub = parser.add_subparsers(dest="command")

    prepare = sub.add_parser("prepare")
    prepare.add_argument("--tag", required=True)
    prepare.add_argument("--dir", required=True)
    prepare.add_argument("--out")
    prepare.add_argument("--token", default=os.environ.get("GITHUB_TOKEN"))
    prepare.add_argument("--check-release", action="store_true")
    prepare.set_defaults(func=command_prepare)

    package = sub.add_parser("package-source")
    package.add_argument("--tag", required=True)
    package.add_argument("--build", required=True)
    package.add_argument("--out", required=True)
    package.set_defaults(func=command_package_source)

    publish = sub.add_parser("publish")
    publish.add_argument("--tag", required=True)
    publish.add_argument("--assets", required=True)
    publish.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "TheM14/am-hook-joc"))
    publish.add_argument("--token", default=os.environ.get("GITHUB_TOKEN"))
    publish.add_argument("--dry-run", action="store_true")
    publish.set_defaults(func=command_publish)

    monitor = sub.add_parser("monitor")
    monitor.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "TheM14/am-hook-joc"))
    monitor.add_argument("--token", default=os.environ.get("GITHUB_TOKEN"))
    monitor.add_argument("--dry-run", action="store_true")
    monitor.add_argument("--timeout", type=int, default=3600)
    monitor.set_defaults(func=command_monitor)

    args = parser.parse_args()
    if args.self_test:
        return run_self_test()
    if not args.command:
        parser.print_help()
        return 2
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
