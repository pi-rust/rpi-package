"""Build and publish versioned extension assets using the v1 distribution protocol."""

import argparse
import hashlib
import hmac
import json
import os
import pathlib
import re
import shutil
import subprocess
import tarfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile

GNU_TARGETS = {
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}

# The first musl release is intentionally x86_64-only. The build is native in
# the musl container, which is required for cdylib/unwind and native package
# detection. Add another target only together with a native toolchain and
# runtime smoke test for that architecture.
MUSL_TARGETS = {
    "x86_64-unknown-linux-musl",
}
TARGETS = GNU_TARGETS | MUSL_TARGETS
WEB_TARGETS = GNU_TARGETS


def command(*args):
    return subprocess.check_output(args, encoding="utf-8").strip()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def asset(path, repository, tag):
    return {
        "url": f"https://github.com/{repository}/releases/download/{tag}/{path.name}",
        "sha256": digest(path),
        "size": path.stat().st_size,
        "format": "zip" if path.suffix == ".zip" else "tar.gz",
    }


def platform_slug(target):
    """Return the installer/archive slug without conflating GNU and musl."""
    windows = "windows" in target
    macos = "apple-darwin" in target
    os_name = "windows" if windows else "macos" if macos else "linux"
    arch = target.split("-", 1)[0]
    suffix = "-musl" if target.endswith("-unknown-linux-musl") else ""
    return f"{os_name}-{arch}{suffix}"


def archive_library(library, destination):
    if destination.suffix == ".zip":
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.write(library, library.name)
    else:
        with tarfile.open(destination, "w:gz") as archive:
            archive.add(library, arcname=library.name)


def sdk_identity(package, metadata):
    by_id = {item["id"]: item for item in metadata["packages"]}
    node = next(item for item in metadata["resolve"]["nodes"] if item["id"] == package["id"])
    sdk_id = next(dep["pkg"] for dep in node["deps"] if dep["name"] == "rpi_plugin_sdk")
    sdk = by_id[sdk_id]
    source = pathlib.Path(sdk["manifest_path"]).parent / "src/lib.rs"
    match = re.search(r"pub const RPI_PLUGIN_ABI_VERSION_UNIFIED:\s*u32\s*=\s*(\d+)", source.read_text(encoding="utf-8"))
    if not match:
        raise ValueError(f"Cannot determine the plugin ABI of SDK {sdk['version']}")
    return {"version": sdk["version"], "plugin_abi": int(match.group(1))}


def build(args):
    metadata = json.loads(command("cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", args.target))
    host = re.search(r"^host: (.+)$", command("rustc", "-vV"), re.MULTILINE).group(1)
    if host != args.target:
        raise ValueError(f"Native build target {host} does not match {args.target}")
    revision = command("git", "rev-parse", "HEAD")
    if revision != args.source_ref:
        raise ValueError("The build checkout differs from the release commit")
    windows = "windows" in args.target
    macos = "apple-darwin" in args.target
    extension = "dll" if windows else "dylib" if macos else "so"
    archive_extension = "zip" if windows else "tar.gz"
    release_dir = pathlib.Path(metadata["target_directory"]) / "release"
    output = pathlib.Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    selected = set(json.loads(args.tags))
    found = set()
    packages = []
    for package in metadata["packages"]:
        if package["id"] not in metadata["workspace_members"]:
            continue
        library_target = next((target for target in package["targets"] if "cdylib" in target["crate_types"]), None)
        if not library_target:
            continue
        tag = f"{package['name']}-v{package['version']}"
        if getattr(args, "packages_only", False) and tag not in selected:
            continue
        stem = library_target["name"] if windows else "lib" + library_target["name"]
        library = release_dir / f"{stem}.{extension}"
        if not library.is_file():
            raise ValueError(f"Missing compiled library: {library}")
        tag = f"{package['name']}-v{package['version']}"
        record = {
            "name": package["name"], "version": package["version"],
            "library": {"path": library.name, "sha256": digest(library)},
            "sdk": sdk_identity(package, metadata),
        }
        packages.append(record)
        if tag not in selected:
            continue
        expected_repository = f"https://github.com/{args.repository}"
        if package["repository"].rstrip("/").removesuffix(".git") != expected_repository:
            raise ValueError(f"{package['name']} repository metadata does not match the publishing repository")
        found.add(tag)
        archive = output / f"{package['name']}-{package['version']}-{args.target}.{archive_extension}"
        archive_library(library, archive)
        distribution = asset(archive, args.repository, tag)
        distribution["library"] = record["library"]
        fragment = {
            "schema_version": 1, "name": package["name"], "version": package["version"],
            "repository": expected_repository, "tag": tag, "source_commit": revision,
            "sdk": record["sdk"], "target": args.target, "distribution": distribution,
        }
        write_json(output / f"{package['name']}.{args.target}.json", fragment)
    if selected != found:
        raise ValueError(f"Release tags do not match workspace versions: {sorted(selected - found)}")
    if getattr(args, "packages_only", False):
        if not selected:
            raise ValueError("Package-only builds require at least one release tag")
        return

    platform = platform_slug(args.target)
    bundle = pathlib.Path("dist") / f"rpi-packages-{platform}.{archive_extension}"
    bridge = pathlib.Path("dist") / f"rpi-acp-{platform}.{archive_extension}"
    # The installer has already produced the bundle and standalone ACP bridge.
    import shutil
    for source in (bundle, bridge):
        shutil.copy2(source, output / source.name)
    write_json(output / f"bundle.{args.target}.json", {
        "schema_version": 1, "repository": f"https://github.com/{args.repository}",
        "tag": args.bundle_tag, "source_commit": revision, "target": args.target,
        "archive": asset(output / bundle.name, args.repository, args.bundle_tag),
        "acp_bridge": asset(output / bridge.name, args.repository, args.bundle_tag),
        "packages": packages,
    })


def release_exists(tag):
    return subprocess.run(["gh", "release", "view", tag], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def check_tag_source(repository, tag, revision):
    from urllib.parse import quote
    target = json.loads(command("gh", "api", f"repos/{repository}/git/ref/tags/{quote(tag, safe='')}"))["object"]
    while target["type"] == "tag":
        target = json.loads(command("gh", "api", f"repos/{repository}/git/tags/{target['sha']}"))["object"]
    if target["type"] != "commit" or target["sha"] != revision:
        raise ValueError(f"Release tag {tag} does not point to the build commit")


def upload(tag, paths):
    subprocess.run(["gh", "release", "upload", tag, *map(str, paths), "--clobber"], check=True)


def collect(root, pattern, source_ref, expected_targets=None):
    expected_targets = TARGETS if expected_targets is None else expected_targets
    fragments = [json.loads(path.read_text(encoding="utf-8")) for path in root.glob(pattern)]
    targets = [fragment["target"] for fragment in fragments]
    if set(targets) != expected_targets or len(targets) != len(expected_targets):
        raise ValueError(f"Missing or duplicate platform artifacts for {pattern}: {targets}")
    if any(fragment["source_commit"] != source_ref for fragment in fragments):
        raise ValueError("Platform artifacts were built from different source commits")
    return fragments


def checked_archive(root, distribution):
    name = distribution["url"].rsplit("/", 1)[1]
    path = root / name
    if digest(path) != distribution["sha256"] or path.stat().st_size != distribution["size"]:
        raise ValueError(f"Archive checksum or size does not match: {name}")
    return path


RUSTFS_CORE_ENV = (
    "RPI_RUSTFS_ENDPOINT",
    "RPI_RUSTFS_BUCKET",
    "RPI_RUSTFS_ACCESS_KEY",
    "RPI_RUSTFS_SECRET_KEY",
)


def rustfs_configuration(environ=None):
    """Read optional mirror settings without ever logging credentials."""
    environ = os.environ if environ is None else environ
    values = {name: environ.get(name, "").strip() for name in RUSTFS_CORE_ENV}
    public_url = environ.get("RPI_CDN_BASE_URL", "").strip()
    if not any(values.values()):
        return None
    missing = [name for name, value in values.items() if not value]
    if missing:
        raise ValueError("RustFS mirror is partially configured; missing " + ", ".join(missing))
    parsed_endpoint = urllib.parse.urlparse(values["RPI_RUSTFS_ENDPOINT"])
    if (parsed_endpoint.scheme not in ("http", "https") or not parsed_endpoint.netloc
            or parsed_endpoint.username or parsed_endpoint.password
            or parsed_endpoint.query or parsed_endpoint.fragment):
        raise ValueError("RPI_RUSTFS_ENDPOINT must be an http(s) URL without credentials or query")
    if public_url:
        parsed_public = urllib.parse.urlparse(public_url)
        if (parsed_public.scheme not in ("http", "https") or not parsed_public.netloc
                or parsed_public.username or parsed_public.password
                or parsed_public.query or parsed_public.fragment):
            raise ValueError("RPI_CDN_BASE_URL must be an http(s) URL without credentials or query")
    values["RPI_CDN_BASE_URL"] = public_url.rstrip("/")
    return values


def _distribution_files(root, distribution):
    path = checked_archive(root, distribution)
    if path.name != pathlib.Path(path.name).name:
        raise ValueError(f"Release asset name must not contain a path: {path.name}")
    files = [path]
    checksum = root / (path.name + ".sha256")
    if checksum.is_file():
        files.append(checksum)
    return files


def mirror_files(root, tags, bundle_tag=None):
    """Return (release tag, local path) pairs for the exact published assets."""
    files = []
    for tag in tags:
        manifest_path = root / "manifests" / tag / "rpi-extension.json"
        if not manifest_path.is_file():
            raise ValueError(f"Missing package discovery manifest for mirror: {manifest_path}")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest.get("tag") != tag:
            raise ValueError(f"Package manifest tag does not match mirror tag: {tag}")
        for distribution in manifest.get("targets", {}).values():
            files.extend((tag, path) for path in _distribution_files(root, distribution))
        files.append((tag, manifest_path))

    if bundle_tag and bundle_tag != "unused":
        manifest_path = root / "rpi-packages.json"
        if not manifest_path.is_file():
            raise ValueError(f"Missing bundle discovery manifest for mirror: {manifest_path}")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest.get("tag") != bundle_tag:
            raise ValueError("Bundle manifest tag does not match mirror tag")
        for target in manifest.get("targets", {}).values():
            for name in ("archive", "acp_bridge"):
                files.extend((bundle_tag, path) for path in _distribution_files(root, target[name]))
        files.append((bundle_tag, manifest_path))
    return files


def _object_key(tag, path):
    return f"rpi-package/releases/{tag}/{path.name}"


def _aws_head_command(aws, config, key):
    return [aws, "s3api", "head-object", "--bucket", config["RPI_RUSTFS_BUCKET"],
            "--key", key, "--endpoint-url", config["RPI_RUSTFS_ENDPOINT"], "--no-cli-pager"]


def _aws_copy_command(aws, config, tag, path):
    key = _object_key(tag, path)
    if path.suffix == ".json":
        content_type = "application/json"
        cache_control = "public,max-age=60,s-maxage=60,must-revalidate"
    elif path.name.endswith(".sha256"):
        content_type = "text/plain"
        cache_control = "public,max-age=31536000,immutable"
    else:
        content_type = "application/octet-stream"
        cache_control = "public,max-age=31536000,immutable"
    return [aws, "s3", "cp", str(path),
            f"s3://{config['RPI_RUSTFS_BUCKET']}/{key}",
            "--endpoint-url", config["RPI_RUSTFS_ENDPOINT"], "--only-show-errors",
            "--content-type", content_type, "--cache-control", cache_control,
            "--metadata", f"sha256={digest(path)}"]


def _is_missing_object(result):
    output = ((result.stdout or "") + "\\n" + (result.stderr or "")).lower()
    return "nosuchkey" in output or "status code: 404" in output or "(404)" in output


def _verify_existing_object(result, path):
    try:
        metadata = json.loads(result.stdout or "{}")
    except json.JSONDecodeError as error:
        raise RuntimeError(f"Cannot verify existing immutable CDN object: {path.name}") from error
    if metadata.get("ContentLength") != path.stat().st_size:
        raise ValueError(f"Immutable CDN object has a different size: {path.name}")
    remote_sha = (metadata.get("Metadata") or {}).get("sha256")
    if remote_sha and remote_sha != digest(path):
        raise ValueError(f"Immutable CDN object has a different checksum: {path.name}")


def _sign_request(config, method, key, body=b""):
    """Minimal S3 Signature V4 request for the no-AWS-CLI fallback."""
    endpoint = urllib.parse.urlparse(config["RPI_RUSTFS_ENDPOINT"])
    now = __import__("datetime").datetime.now(__import__("datetime").timezone.utc)
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    date = now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(body).hexdigest()
    path = "/" + urllib.parse.quote(config["RPI_RUSTFS_BUCKET"] + "/" + key, safe="/-_.~")
    host = endpoint.netloc
    headers = {"host": host, "x-amz-content-sha256": payload_hash, "x-amz-date": amz_date}
    canonical_headers = "".join(f"{name}:{headers[name]}\n" for name in sorted(headers))
    signed_headers = ";".join(sorted(headers))
    canonical = "\n".join((method, path, "", canonical_headers, signed_headers, payload_hash))
    scope = f"{date}/us-east-1/s3/aws4_request"
    string_to_sign = "\n".join(("AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()))
    def sign(key_bytes, value):
        return hmac.new(key_bytes, value.encode(), hashlib.sha256).digest()
    signing_key = sign(sign(sign(sign(("AWS4" + config["RPI_RUSTFS_SECRET_KEY"]).encode(), date), "us-east-1"), "s3"), "aws4_request")
    signature = hmac.new(signing_key, string_to_sign.encode(), hashlib.sha256).hexdigest()
    headers["Authorization"] = f"AWS4-HMAC-SHA256 Credential={config['RPI_RUSTFS_ACCESS_KEY']}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    return urllib.parse.urlunparse((endpoint.scheme, endpoint.netloc, path, "", "", "")), headers


def _stdlib_mirror(config, tag, path):
    key = _object_key(tag, path)
    try:
        url, headers = _sign_request(config, "HEAD", key)
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers, method="HEAD")) as response:
            length = response.headers.get("Content-Length")
            if length and int(length) != path.stat().st_size:
                raise ValueError(f"Immutable CDN object has a different size: {path.name}")
            remote_sha = response.headers.get("x-amz-meta-sha256")
            if remote_sha and remote_sha != digest(path):
                raise ValueError(f"Immutable CDN object has a different checksum: {path.name}")
            print(f"Preserving existing immutable CDN object: {key}")
            return
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise RuntimeError(f"CDN HEAD request failed for {key}: HTTP {error.code}") from error
    body = path.read_bytes()
    url, headers = _sign_request(config, "PUT", key, body)
    headers["content-type"] = "application/json" if path.suffix == ".json" else (
        "text/plain" if path.name.endswith(".sha256") else "application/octet-stream")
    headers["cache-control"] = ("public,max-age=60,s-maxage=60,must-revalidate"
                                 if path.suffix == ".json" else "public,max-age=31536000,immutable")
    headers["x-amz-meta-sha256"] = digest(path)
    headers["content-length"] = str(len(body))
    request = urllib.request.Request(url, data=body, headers=headers, method="PUT")
    with urllib.request.urlopen(request):
        pass


def mirror(args):
    config = rustfs_configuration()
    if config is None:
        print("RustFS mirror not configured; skipping optional CDN mirror.")
        return
    root = pathlib.Path(args.output)
    files = mirror_files(root, json.loads(args.tags), args.bundle_tag)
    base = config["RPI_CDN_BASE_URL"]
    destination = base + "/rpi-package/releases/" if base else "configured S3 endpoint"
    print(f"RustFS mirror enabled for {len(files)} immutable release files ({destination})")
    aws = shutil.which("aws")
    if args.dry_run:
        for tag, path in files:
            print("DRY-RUN", " ".join(_aws_copy_command(aws or "aws", config, tag, path)))
        return
    if aws:
        command_env = os.environ.copy()
        command_env.update({"AWS_ACCESS_KEY_ID": config["RPI_RUSTFS_ACCESS_KEY"],
                            "AWS_SECRET_ACCESS_KEY": config["RPI_RUSTFS_SECRET_KEY"],
                            "AWS_EC2_METADATA_DISABLED": "true"})
        for tag, path in files:
            key = _object_key(tag, path)
            head = subprocess.run(_aws_head_command(aws, config, key), env=command_env,
                                  capture_output=True, text=True)
            if head.returncode == 0:
                _verify_existing_object(head, path)
                print(f"Preserving existing immutable CDN object: {key}")
                continue
            if not _is_missing_object(head):
                raise RuntimeError(f"Cannot inspect CDN object before immutable upload: {key}")
            subprocess.run(_aws_copy_command(aws, config, tag, path), env=command_env, check=True)
    else:
        print("AWS CLI unavailable; using the standard-library S3 uploader.")
        for tag, path in files:
            _stdlib_mirror(config, tag, path)


def publish(args):
    root = pathlib.Path(args.output)
    # Validate all platforms and prepare all manifests before any upload.
    prepared = []
    for tag in json.loads(args.tags):
        name, version = tag.rsplit("-v", 1)
        fragments = collect(root, f"{name}.*.json", args.source_ref)
        first = fragments[0]
        identity = (name, version, first["repository"], tag, first["sdk"])
        for fragment in fragments:
            if (fragment["name"], fragment["version"], fragment["repository"], fragment["tag"], fragment["sdk"]) != identity:
                raise ValueError(f"Platform package identity differs for {tag}")
        manifest = {key: first[key] for key in ("schema_version", "name", "version", "repository", "tag", "source_commit", "sdk")}
        manifest["targets"] = {fragment["target"]: fragment["distribution"] for fragment in fragments}
        path = root / "manifests" / tag / "rpi-extension.json"
        write_json(path, manifest)
        paths = [checked_archive(root, fragment["distribution"]) for fragment in fragments]
        prepared.append((tag, path, paths))

    if getattr(args, "packages_only", False):
        if not prepared:
            raise ValueError("Package-only publishing requires at least one release tag")
        for tag, _, _ in prepared:
            check_tag_source(args.repository, tag, args.source_ref)
            if not release_exists(tag):
                raise ValueError(f"Package Release must exist before uploading assets: {tag}")
        for tag, manifest, paths in prepared:
            existing = json.loads(command("gh", "release", "view", tag, "--json", "assets"))["assets"]
            if any(item["name"] == "rpi-extension.json" for item in existing):
                print(f"Preserving already published immutable assets: {tag}")
                continue
            upload(tag, paths)
            upload(tag, [manifest])
        return

    bundles = collect(root, "bundle.*.json", args.source_ref)
    bundle_manifest = {
        "schema_version": 1, "repository": f"https://github.com/{args.repository}",
        "tag": args.bundle_tag, "source_commit": args.source_ref,
        "targets": {fragment["target"]: {key: fragment[key] for key in ("archive", "acp_bridge", "packages")} for fragment in bundles},
    }
    bundle_paths = []
    for fragment in bundles:
        if fragment["tag"] != args.bundle_tag:
            raise ValueError("Bundle tags differ between platforms")
        bundle_paths.extend(checked_archive(root, fragment[key]) for key in ("archive", "acp_bridge"))
    bundle_manifest_path = root / "rpi-packages.json"
    write_json(bundle_manifest_path, bundle_manifest)

    for tag, _, _ in prepared:
        check_tag_source(args.repository, tag, args.source_ref)
    bundle_exists = release_exists(args.bundle_tag)
    if bundle_exists:
        check_tag_source(args.repository, args.bundle_tag, args.source_ref)

    for tag, manifest, paths in prepared:
        if not release_exists(tag):
            raise ValueError(f"Package Release must exist before uploading assets: {tag}")
        existing_assets = json.loads(command("gh", "release", "view", tag, "--json", "assets"))["assets"]
        if any(item["name"] == "rpi-extension.json" for item in existing_assets):
            print(f"Preserving already published immutable assets: {tag}")
            continue
        upload(tag, paths)
        # Publish the discovery manifest last, after all supported archives exist.
        upload(tag, [manifest])

    if not bundle_exists:
        subprocess.run(["gh", "release", "create", args.bundle_tag, "--target", args.source_ref,
                        "--title", f"Extension bundle {args.source_ref[:12]}", "--generate-notes", "--latest=false"], check=True)
    bundle_assets = json.loads(command("gh", "release", "view", args.bundle_tag, "--json", "assets"))["assets"]
    if not any(item["name"] == "rpi-packages.json" for item in bundle_assets):
        upload(args.bundle_tag, bundle_paths)
        upload(args.bundle_tag, [bundle_manifest_path])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "publish", "mirror"))
    parser.add_argument("--target", choices=sorted(TARGETS))
    parser.add_argument("--repository")
    parser.add_argument("--source-ref")
    parser.add_argument("--bundle-tag", default="unused")
    parser.add_argument("--tags", default="[]", help="JSON array of per-package release tags")
    parser.add_argument("--packages-only", action="store_true", help="Build/publish selected packages without the full workspace bundle")
    parser.add_argument("--output", default="dist/release-assets")
    parser.add_argument("--dry-run", action="store_true", help="Print mirror commands without uploading")
    args = parser.parse_args()
    if args.command == "build" and (not args.target or not args.repository or not args.source_ref):
        parser.error("build requires --target, --repository and --source-ref")
    if args.command == "publish" and (not args.repository or not args.source_ref):
        parser.error("publish requires --repository and --source-ref")
    (build if args.command == "build" else publish if args.command == "publish" else mirror)(args)


if __name__ == "__main__":
    main()
