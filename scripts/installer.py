#!/usr/bin/env python3
"""从已验证的 Flower ZIP 构建离线 Inno Setup 安装器，不执行安装器。"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Callable, Sequence

if __package__:
    from . import package as pack
else:
    import package as pack

REPO = Path(__file__).resolve().parent.parent
ROOT_FILES = pack.CATEGORY_FILES | frozenset({
    "flower.dll", "flower-register.exe", "flower-settings.exe", "README.md",
    "LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md", "Cargo.lock",
    "manifest.json", "SHA256SUMS", "data/pinyin/mvp_lexicon.tsv",
    "data/pinyin/base_lexicon.tsv", "data/pinyin/coverage.json",
    "data/pinyin/everyday_phrases.tsv", "data/pinyin/everyday_phrases.md",
    "data/pinyin/sources/manifest.json", "data/pinyin/sources/kMandarin_8105.txt",
    "data/pinyin/phrases/phrase_lexicon.tsv", "data/pinyin/phrases/proper_lexicon.tsv",
    "data/pinyin/phrases/coverage.json", "data/pinyin/phrases/manifest.json",
    "data/pinyin/phrases/quality-cases.json",
})
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.+-]+)?\Z")
MAX_PAYLOAD = 512 * 1024 * 1024
Runner = Callable[[Sequence[str], Path], subprocess.CompletedProcess[str]]
InstallerError = pack.PackagingError


@dataclass(frozen=True)
class VerifiedPackage:
    version: str
    digest: str
    payload: dict[str, bytes]
    binaries: list[dict]
    variant: str = "base"


@dataclass(frozen=True)
class InstallerResult:
    executable: Path
    checksum: Path
    manifest: Path


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _safe_relative(value: str) -> bool:
    if not value or "\\" in value or any(ord(char) < 32 for char in value):
        return False
    parts = value.split("/")
    reserved = {"CON", "PRN", "AUX", "NUL"}
    reserved.update(f"{prefix}{i}" for prefix in ("COM", "LPT") for i in range(1, 10))
    reserved.update(f"{prefix}{i}" for prefix in ("COM", "LPT") for i in "¹²³")
    return all(
        part not in ("", ".", "..")
        and not any(char in part for char in '<>:"|?*{}')
        and not part.endswith((" ", "."))
        and part.split(".")[0].rstrip(" ").upper() not in reserved
        for part in parts
    )


def verify_package(archive: Path) -> VerifiedPackage:
    """校验完整 ZIP 快照，不使用 extractall 或加载二进制。"""
    with archive.open("rb") as stream:
        data = stream.read(MAX_PAYLOAD + 1)
    if len(data) > MAX_PAYLOAD:
        raise InstallerError("ZIP 超过安装器允许的大小")
    digest = _digest(data)
    with Path(f"{archive}.sha256").open("rb") as stream:
        checksum_bytes = stream.read(8193)
    if len(checksum_bytes) > 8192:
        raise InstallerError("ZIP 外部校验文件过大")
    checksum = checksum_bytes.decode("utf-8").strip()
    match = re.fullmatch(r"([0-9a-fA-F]{64})  ([^\r\n]+)", checksum)
    if match is None or match[2] != archive.name or match[1].lower() != digest:
        raise InstallerError("ZIP 外部 SHA-256 不匹配")
    import io

    payload: dict[str, bytes] = {}
    seen: set[str] = set()
    top: str | None = None
    total = 0
    with zipfile.ZipFile(io.BytesIO(data)) as source:
        entries = source.infolist()
        if len(entries) > 4096:
            raise InstallerError("ZIP 条目过多")
        for entry in entries:
            name = entry.filename
            if name != entry.orig_filename or not _safe_relative(name) or entry.is_dir():
                raise InstallerError(f"ZIP 路径不安全或包含非文件条目：{name}")
            if (stat.S_IFMT(entry.external_attr >> 16) not in (0, stat.S_IFREG)
                    or entry.external_attr & 0x400):
                raise InstallerError("ZIP 包含符号链接或其他非普通文件")
            components = name.split("/", 1)
            if len(components) != 2:
                raise InstallerError("ZIP 必须只有一个顶层目录")
            if top is None:
                top = components[0]
            if components[0] != top or name.casefold() in seen:
                raise InstallerError("ZIP 存在多个顶层或大小写冲突的重复条目")
            seen.add(name.casefold())
            relative = components[1]
            if (relative not in ROOT_FILES
                    and relative not in pack.VOCABULARY_FILES
                    and relative != pack.VOCABULARY_ARCHIVE
                    and relative not in pack.CONVERSION_FILES
                    and relative not in {"ai/" + name for name in pack.package_ai_assets.BUNDLE_FILES}
                    and not relative.startswith("licenses/")):
                raise InstallerError(f"ZIP 存在白名单之外的文件：{relative}")
            if relative.startswith("licenses/") and Path(relative).suffix.lower() in (
                ".exe", ".dll", ".ps1", ".bat", ".cmd", ".com",
            ):
                raise InstallerError("许可证目录不能包含可执行文件")
            total += entry.file_size
            if entry.file_size > MAX_PAYLOAD or total > MAX_PAYLOAD or entry.flag_bits & 1:
                raise InstallerError("ZIP payload 过大或已加密")
            payload[relative] = source.read(entry)
    if not ROOT_FILES.issubset(payload):
        raise InstallerError("ZIP 缺少必需 payload")
    manifest = json.loads(payload["manifest.json"])
    if not isinstance(manifest, dict):
        raise InstallerError("manifest 必须是 JSON 对象")
    version = manifest.get("version")
    if not isinstance(version, str) or not VERSION.fullmatch(version) or len(version) > 127:
        raise InstallerError("包版本不是安全的 Cargo 版本字符串")
    if any(int(part) > 65535 for part in re.split(r"[-+]", version, maxsplit=1)[0].split(".")):
        raise InstallerError("版本超出 Windows 版本资源允许的范围")
    if (
        manifest.get("package") != "flower"
        or manifest.get("target") != pack.TARGET
        or manifest.get("schema_version") != 1
        or top != f"flower-{version}-windows-x64"
    ):
        raise InstallerError("manifest 身份、目标或顶层目录不匹配")
    variant = manifest.get("variant", "base")
    release = tuple(int(part) for part in re.split(r"[-+]", version, maxsplit=1)[0].split("."))
    if (release >= (0, 4, 21) or "expanded_vocabulary" in manifest
            or pack.VOCABULARY_ARCHIVE in payload
            or any(path.startswith(pack.VOCABULARY_PREFIX) for path in payload)):
        try:
            if release >= (0, 4, 22):
                vocabulary_record = pack._verify_vocabulary_archive(payload)
            else:
                if pack.VOCABULARY_ARCHIVE in payload:
                    raise InstallerError("旧版本不能携带审计 ZIP")
                vocabulary_record = pack._verify_vocabulary_payload(payload)
        except pack.PackagingError as error:
            raise InstallerError(f"扩充词库校验失败：{error}") from error
        if manifest.get("expanded_vocabulary") != vocabulary_record:
            raise InstallerError("manifest 扩充词库记录不匹配")
    if (release >= (0, 4, 22) or "chinese_conversion" in manifest
            or any(p.startswith(pack.CONVERSION_PREFIX) for p in payload)):
        conversion_record = pack._verify_conversion_payload(payload)
        if manifest.get("chinese_conversion") != conversion_record:
            raise InstallerError("manifest 简繁转换记录不匹配")
    if release >= (0, 4, 7) and variant != "ai":
        raise InstallerError("0.4.7 起必须提供完整 AI 分发包")
    ai_payload = {name[3:]: value for name, value in payload.items() if name.startswith("ai/")}
    if variant == "ai":
        try:
            ai_record = pack.package_ai_assets.verify_payload(ai_payload, REPO)
        except (OSError, ValueError, KeyError, TypeError) as error:
            raise InstallerError(f"AI payload 校验失败：{error}") from error
        if manifest.get("ai") != ai_record:
            raise InstallerError("manifest AI 记录与固定源指纹不匹配")
    elif variant != "base" or ai_payload or manifest.get("ai", {"available": False}) != {"available": False}:
        raise InstallerError("普通包不能包含 AI 资产或可用记录")
    if archive.name != pack._package_paths(version, variant)[1]:
        raise InstallerError("ZIP 文件名与 manifest 变体不匹配")
    hashes: dict[str, str] = {}
    for line in payload["SHA256SUMS"].decode("utf-8").splitlines():
        row = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        if row is None or not _safe_relative(row[2]) or row[2] in hashes:
            raise InstallerError("SHA256SUMS 格式无效或包含重复项")
        hashes[row[2]] = row[1]
    if set(hashes) != set(payload) - {"SHA256SUMS"}:
        raise InstallerError("SHA256SUMS 没有精确覆盖全部 payload")
    if any(_digest(payload[name]) != value for name, value in hashes.items()):
        raise InstallerError("包内 payload 哈希不匹配")
    records = manifest.get("binaries")
    roles = {
        "flower.dll": "text_service_dll",
        "flower-register.exe": "registration_cli",
        "flower-settings.exe": "settings_ui",
    }
    if not isinstance(records, list) or len(records) != 3:
        raise InstallerError("manifest 二进制清单无效")
    if {row.get("path") for row in records if isinstance(row, dict)} != set(roles):
        raise InstallerError("manifest 二进制路径无效")
    for record in records:
        name = record["path"]
        if (
            record.get("role") != roles[name]
            or record.get("size") != len(payload[name])
            or record.get("sha256") != hashes[name]
        ):
            raise InstallerError(f"manifest 二进制元数据不匹配：{name}")
        image = pack._parse_pe(payload[name], name)
        if name == "flower.dll" and not pack.EXPECTED_DLL_EXPORTS.issubset(
            pack._export_names(image, payload[name])
        ):
            raise InstallerError("DLL 缺少必需导出")
    if manifest.get("cargo_lock_sha256") != hashes["Cargo.lock"]:
        raise InstallerError("Cargo.lock 哈希与 manifest 不匹配")
    if manifest.get("category_lexicons") != pack._verify_category_payload(payload, REPO):
        raise InstallerError("manifest 分类词库记录不匹配")
    return VerifiedPackage(version, digest, payload, records, variant)


def find_compiler(explicit: Path | None) -> Path:
    if explicit is not None:
        path = explicit.expanduser().resolve()
        if not path.is_file():
            raise InstallerError("--iscc 不是已存在的编译器文件")
        return path
    found = shutil.which("ISCC.exe")
    if found:
        return Path(found)
    try:
        import winreg

        uninstall = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
        for root in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
            for view in (winreg.KEY_WOW64_64KEY, winreg.KEY_WOW64_32KEY):
                try:
                    with winreg.OpenKey(root, uninstall, 0, winreg.KEY_READ | view) as key:
                        for index in range(winreg.QueryInfoKey(key)[0]):
                            with winreg.OpenKey(key, winreg.EnumKey(key, index)) as child:
                                try:
                                    name = winreg.QueryValueEx(child, "DisplayName")[0]
                                    location = winreg.QueryValueEx(child, "InstallLocation")[0]
                                except OSError:
                                    continue
                                if str(name).startswith("Inno Setup version 6"):
                                    path = Path(location) / "ISCC.exe"
                                    if path.is_file():
                                        return path
                except OSError:
                    continue
    except ImportError:
        pass
    raise InstallerError("找不到 Inno Setup 6 ISCC.exe；请安装或用 --iscc 指定路径")


def _run(args: Sequence[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args, cwd=cwd, capture_output=True, text=True,
        encoding="utf-8", errors="replace", shell=False,
    )


def build_installer(
    archive: Path, output_dir: Path, *, compiler: Path | None = None,
    repo: Path = REPO, runner: Runner = _run, host: str = sys.platform,
) -> InstallerResult:
    if host != "win32":
        raise InstallerError("真实 EXE 安装器仅在 Windows 上构建")
    verified = verify_package(archive)
    if verified.variant != "ai":
        raise InstallerError("新安装器只能从完整 AI 分发包构建")
    iscc = find_compiler(compiler)
    engine_license = (iscc.parent / "license.txt").read_bytes()
    if not engine_license.startswith(b"Inno Setup License") or b"Jordan Russell" not in engine_license:
        raise InstallerError("缺少可识别的 Inno Setup 编译器许可证正文")
    stem = f"flower-{verified.version}-windows-x64-setup"
    requested_output = output_dir.expanduser().absolute()
    if requested_output.is_symlink():
        raise InstallerError("输出目录不能是符号链接")
    output = requested_output.resolve()
    result = InstallerResult(
        output / f"{stem}.exe", output / f"{stem}.exe.sha256",
        output / f"{stem}.manifest.json",
    )
    for destination in (result.executable, result.checksum, result.manifest):
        if os.path.lexists(destination):
            raise InstallerError(f"已有输出，拒绝覆盖：{destination.name}")
    output.mkdir(parents=True, exist_ok=True)
    published: list[pack.PublishedFile] = []
    with tempfile.TemporaryDirectory(prefix=".flower-installer-", dir=output) as staging:
        stage = Path(staging)
        payload = stage / "payload"
        payload.mkdir()
        lines = []
        for relative, data in sorted(verified.payload.items()):
            path = payload.joinpath(*PurePosixPath(relative).parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            windows_path = relative.replace("/", "\\")
            parent = str(PurePosixPath(relative).parent)
            suffix = "" if parent == "." else "\\" + parent.replace("/", "\\")
            lines.append(
                f'Source: "{{#PackageDir}}\\{windows_path}"; '
                f'DestDir: "{{app}}\\versions\\{{#BuildId}}{suffix}"; Flags: ignoreversion'
            )
        include = stage / "payload.iss"
        include.write_text("\n".join(lines) + "\n", encoding="utf-8-sig")
        license_snapshot = stage / "INNO-SETUP-LICENSE.txt"
        license_snapshot.write_bytes(engine_license)
        build_id = f"{verified.version}-{verified.digest[:12]}"
        file_version = re.split(r"[-+]", verified.version, maxsplit=1)[0] + ".0"
        exe = stage / f"{stem}.exe"
        args = [
            str(iscc), "/Q",
            f"/F{stem}",
            f"/DAppVersion={verified.version}", f"/DBuildId={build_id}",
            f"/DFileVersion={file_version}",
            f"/DPackageDir={payload}", f"/DFilesInclude={include}",
            f"/DOutputDir={stage}",
            f"/DExpectedDllHash={_digest(verified.payload['flower.dll'])}",
            f"/DExpectedToolHash={_digest(verified.payload['flower-register.exe'])}",
            f"/DExpectedSettingsHash={_digest(verified.payload['flower-settings.exe'])}",
            f"/DEngineLicense={license_snapshot}",
            str(repo / "installer" / "flower.iss"),
        ]
        completed = runner(args, repo)
        if completed.returncode != 0:
            raise InstallerError(
                "Inno Setup 编译失败："
                + (completed.stderr or completed.stdout or "")[-6000:]
            )
        executable = exe.read_bytes()
        # Inno Setup's bootstrapper may be PE32; the bundled payload is x64.
        if len(executable) < 64 or executable[:2] != b"MZ":
            raise InstallerError("编译器没有生成 Windows PE 安装器")
        offset = int.from_bytes(executable[0x3C:0x40], "little")
        if offset < 64 or offset + 24 > len(executable) or executable[offset:offset + 4] != b"PE\0\0":
            raise InstallerError("安装器 PE 头无效")
        machine = int.from_bytes(executable[offset + 4:offset + 6], "little")
        flags = int.from_bytes(executable[offset + 22:offset + 24], "little")
        if machine not in (0x14C, 0x8664) or flags & 0x2000 or not flags & 2:
            raise InstallerError("安装器不是 x86/x64 Windows EXE")
        exe_hash = _digest(executable)
        checksum = stage / result.checksum.name
        checksum.write_text(f"{exe_hash}  {exe.name}\n", encoding="utf-8")
        manifest = stage / result.manifest.name
        manifest.write_text(json.dumps({
            "schema_version": 1, "package": "flower", "version": verified.version,
            "format": "inno-setup-exe", "payload_target": pack.TARGET,
            "variant": verified.variant,
            "source_zip": archive.name,
            "source_zip_sha256": verified.digest, "build_id": build_id,
            "installer": {"file": exe.name, "size": len(executable), "sha256": exe_hash},
            "authenticode_signed": False, "windows_desktop_verified": False,
            "installation_runtime_verified": False,
            "inno_setup_license_sha256": _digest(engine_license),
        }, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        try:
            for source, destination in (
                (exe, result.executable), (checksum, result.checksum),
                (manifest, result.manifest),
            ):
                published.append(pack._publish_no_overwrite(source, destination))
        except InstallerError as error:
            cleanup = pack._rollback_published(published)
            if cleanup:
                raise InstallerError(f"{error}；回滚错误：{'；'.join(cleanup)}") from error
            raise
    return result


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True, help="已验证 ZIP，旁边必须有 .sha256")
    parser.add_argument("--output-dir", type=Path, default=REPO / "dist")
    parser.add_argument("--iscc", type=Path, help="Inno Setup 6 ISCC.exe 路径")
    args = parser.parse_args(argv)
    try:
        result = build_installer(args.package, args.output_dir, compiler=args.iscc)
    except (InstallerError, OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile) as error:
        print(f"安装器构建失败：{error}", file=sys.stderr)
        return 1
    print(f"已生成：{result.executable}\n校验值：{result.checksum}")
    print("未签名测试版；未执行安装器，也未改变系统注册。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
