#!/usr/bin/env python3
"""构建 Flower Windows x64 ZIP 分发包。"""

from __future__ import annotations

import argparse
import errno
import hashlib
import io
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Mapping, Sequence

try:
    from scripts import dictionary_data, package_ai_assets
except ModuleNotFoundError:
    import dictionary_data
    import package_ai_assets


TARGET = "x86_64-pc-windows-msvc"
EXPECTED_ROOT_LICENSE = "MIT OR Apache-2.0"
EXPECTED_DLL_EXPORTS = frozenset({"DllGetClassObject", "DllCanUnloadNow"})
SAFE_FILE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}\Z")
SUPPORTED_LICENSE_IDS = frozenset(
    {"MIT", "Apache-2.0", "Unlicense", "Unicode-3.0", "ISC", "CC-BY-SA-4.0", "Zlib", "0BSD"}
)
LICENSE_FILE_PREFIXES = (
    "LICENSE",
    "COPYING",
    "NOTICE",
    "COPYRIGHT",
    "UNLICENSE",
)


class PackagingError(Exception):
    """打包失败；消息面向命令行的中文用户。"""


class _ChineseHelpFormatter(argparse.HelpFormatter):
    def add_usage(self, usage, actions, groups, prefix=None):
        super().add_usage(
            usage,
            actions,
            groups,
            prefix="用法：" if prefix is None else prefix,
        )


def _localize_argument_error(message: str) -> str:
    match = re.fullmatch(r"unrecognized arguments: (.+)", message)
    if match:
        return f"无法识别的参数：{match.group(1)}"
    match = re.fullmatch(r"argument (.+): expected one argument", message)
    if match:
        return f"参数 {match.group(1)} 缺少值"
    match = re.fullmatch(r"the following arguments are required: (.+)", message)
    if match:
        return f"缺少必需参数：{match.group(1)}"
    return f"命令行参数无效：{message}"


class _ChineseArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        self.print_usage(sys.stderr)
        self.exit(2, f"{self.prog}: 错误：{_localize_argument_error(message)}\n")


@dataclass(frozen=True)
class Section:
    virtual_address: int
    virtual_size: int
    raw_offset: int
    raw_size: int

    @property
    def mapped_size(self) -> int:
        return max(self.virtual_size, self.raw_size)


@dataclass(frozen=True)
class PeImage:
    sections: tuple[Section, ...]
    size_of_image: int
    export_directory: tuple[int, int]


@dataclass(frozen=True)
class ArtifactPaths:
    dll: Path
    settings: Path
    register: Path


@dataclass(frozen=True)
class PackageResult:
    zip_path: Path
    checksum_path: Path
    version: str


@dataclass(frozen=True)
class PublishedFile:
    path: Path
    device: int
    inode: int


Runner = Callable[[Sequence[str], Path], subprocess.CompletedProcess[str]]


def _subprocess_runner(
    args: Sequence[str], cwd: Path
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(args),
        cwd=str(cwd),
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        shell=False,
    )


def _invoke(
    runner: Runner, args: Sequence[str], cwd: Path, description: str
) -> subprocess.CompletedProcess[str]:
    try:
        return runner(args, cwd)
    except OSError as error:
        raise PackagingError(f"{description}无法启动：{error}") from error


def _checked_output(
    runner: Runner, args: Sequence[str], cwd: Path, description: str
) -> str:
    completed = _invoke(runner, args, cwd, description)
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()
        if len(detail) > 6000:
            detail = detail[-6000:]
        suffix = f"\n{detail}" if detail else ""
        raise PackagingError(
            f"{description}失败（退出码 {completed.returncode}）。{suffix}"
        )
    return completed.stdout or ""


def _read_bytes(path: Path, description: str) -> bytes:
    try:
        data = path.read_bytes()
    except OSError as error:
        raise PackagingError(f"无法读取{description} {path}：{error}") from error
    if not data:
        raise PackagingError(f"{description}为空：{path}")
    return data


def _safe_component(value: object, description: str) -> str:
    if (
        not isinstance(value, str)
        or not SAFE_FILE_NAME.fullmatch(value)
        or value.endswith(".")
    ):
        raise PackagingError(
            f"{description}不能安全用作文件名：{value!r}（仅允许 ASCII 字母、"
            "数字、点、下划线、加号和连字符，长度 1-128，且不得以点结尾）"
        )
    return value


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _package_paths(version: str, variant: str = "base") -> tuple[str, str, str]:
    if variant not in ("base", "ai"):
        raise PackagingError(f"未知分发变体：{variant!r}")
    base = f"flower-{version}-windows-x64"
    release = tuple(int(part) for part in re.split(r"[-+]", version, maxsplit=1)[0].split("."))
    stem = base + ("-ai" if variant == "ai" and release < (0, 4, 7) else "")
    return base, f"{stem}.zip", f"{stem}.zip.sha256"


def _exists_even_dangling(path: Path) -> bool:
    return os.path.lexists(os.fspath(path))


def _reject_existing_outputs(zip_path: Path, checksum_path: Path) -> None:
    existing = [
        path for path in (zip_path, checksum_path) if _exists_even_dangling(path)
    ]
    if existing:
        names = "、".join(path.name for path in existing)
        raise PackagingError(
            f"输出已存在，拒绝覆盖：{names}。请更换 --output-dir 或先人工处理"
            "已有文件（包括悬空符号链接）。"
        )


def _load_metadata(stdout: str) -> tuple[dict, dict, dict[str, dict]]:
    try:
        metadata = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise PackagingError(f"cargo metadata 未返回合法 JSON：{error}") from error
    if not isinstance(metadata, dict):
        raise PackagingError("cargo metadata 的顶层值不是对象")

    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict):
        raise PackagingError("cargo metadata 缺少 resolve 信息")
    root_id = resolve.get("root")
    if not isinstance(root_id, str) or not root_id:
        raise PackagingError("cargo metadata 未给出根包 package_id")

    packages = metadata.get("packages")
    if not isinstance(packages, list):
        raise PackagingError("cargo metadata 缺少 packages")
    package_by_id: dict[str, dict] = {}
    for package in packages:
        if not isinstance(package, dict):
            raise PackagingError("cargo metadata 包含无效 package 项")
        package_id = package.get("id")
        if not isinstance(package_id, str) or not package_id:
            raise PackagingError("cargo metadata 包含缺少 id 的 package")
        if package_id in package_by_id:
            raise PackagingError(f"cargo metadata 包含重复 package_id：{package_id}")
        package_by_id[package_id] = package

    root = package_by_id.get(root_id)
    if root is None:
        raise PackagingError("cargo metadata 的根 package_id 不在 packages 中")
    if root.get("name") != "flower":
        raise PackagingError(
            f"cargo metadata 根包名称必须为 'flower'，实际为 {root.get('name')!r}"
        )
    if root.get("license") != EXPECTED_ROOT_LICENSE:
        raise PackagingError(
            "根 Cargo 包 SPDX 表达式必须为 "
            f"{EXPECTED_ROOT_LICENSE!r}，实际为 {root.get('license')!r}"
        )

    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        raise PackagingError("cargo metadata 缺少 resolve.nodes")
    node_by_id: dict[str, dict] = {}
    for node in nodes:
        if not isinstance(node, dict):
            raise PackagingError("cargo metadata 包含无效 resolve node")
        package_id = node.get("id")
        if not isinstance(package_id, str) or not package_id:
            raise PackagingError("cargo metadata 的 resolve node 缺少 id")
        if package_id in node_by_id:
            raise PackagingError(f"cargo metadata 包含重复 resolve node：{package_id}")
        node_by_id[package_id] = node
    if root_id not in node_by_id:
        raise PackagingError("cargo metadata 的 resolve 缺少根包节点")

    return metadata, root, package_by_id


def _resolved_dependencies(
    metadata: dict, root_id: str, package_by_id: Mapping[str, dict]
) -> list[dict]:
    resolve = metadata["resolve"]
    node_by_id = {node["id"]: node for node in resolve["nodes"]}
    pending = [root_id]
    seen: set[str] = set()
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        node = node_by_id.get(package_id)
        if node is None:
            raise PackagingError(f"cargo resolve 缺少依赖节点：{package_id}")
        dependency_entries: list[object] = []
        for field_name in ("deps", "dependencies"):
            value = node.get(field_name, [])
            if not isinstance(value, list):
                raise PackagingError(
                    f"依赖节点 {field_name} 格式无效：{package_id}"
                )
            dependency_entries.extend(value)
        for dependency in dependency_entries:
            dependency_id = (
                dependency.get("pkg")
                if isinstance(dependency, dict)
                else dependency
            )
            if not isinstance(dependency_id, str) or not dependency_id:
                raise PackagingError(f"依赖节点缺少 package_id：{package_id}")
            pending.append(dependency_id)

    dependencies = []
    for package_id in sorted(seen - {root_id}):
        package = package_by_id.get(package_id)
        if package is None:
            raise PackagingError(f"依赖 package_id 不在 packages 中：{package_id}")
        dependencies.append(package)
    dependencies.sort(
        key=lambda package: (
            str(package.get("name", "")),
            str(package.get("version", "")),
            str(package.get("id", "")),
        )
    )
    return dependencies


def _license_expression_terms(expression: object, label: str) -> set[str]:
    if not isinstance(expression, str) or not expression.strip():
        raise PackagingError(f"{label}缺少 SPDX 许可表达式")
    if expression == "MIT/Apache-2.0":
        expression = "MIT OR Apache-2.0"

    tokens: list[str] = []
    position = 0
    token_pattern = re.compile(
        r"\s*(?:(AND|OR)|(\()|(\))|([A-Za-z0-9][A-Za-z0-9.+-]*))"
    )
    while position < len(expression):
        match = token_pattern.match(expression, position)
        if match is None:
            remaining = expression[position:].strip()
            if not remaining:
                break
            raise PackagingError(f"{label}的 SPDX 表达式无法解析：{expression!r}")
        tokens.append(next(group for group in match.groups() if group is not None))
        position = match.end()
    if not tokens:
        raise PackagingError(f"{label}缺少 SPDX 许可表达式")

    index = 0
    terms: set[str] = set()

    def parse_atom() -> None:
        nonlocal index
        if index >= len(tokens):
            raise PackagingError(f"{label}的 SPDX 表达式不完整：{expression!r}")
        token = tokens[index]
        if token == "(":
            index += 1
            parse_or()
            if index >= len(tokens) or tokens[index] != ")":
                raise PackagingError(f"{label}的 SPDX 表达式括号不匹配")
            index += 1
        elif token in {"AND", "OR", ")"}:
            raise PackagingError(f"{label}的 SPDX 表达式位置无效：{expression!r}")
        else:
            terms.add(token)
            index += 1

    def parse_and() -> None:
        nonlocal index
        parse_atom()
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            parse_atom()

    def parse_or() -> None:
        nonlocal index
        parse_and()
        while index < len(tokens) and tokens[index] == "OR":
            index += 1
            parse_and()

    parse_or()
    if index != len(tokens):
        raise PackagingError(f"{label}的 SPDX 表达式存在多余内容：{expression!r}")
    unknown = terms - SUPPORTED_LICENSE_IDS
    if unknown:
        names = "、".join(sorted(unknown))
        raise PackagingError(
            f"{label}含无法从本地正文确认的 SPDX 标识：{names}。"
            "打包器不会下载许可证，也不会只打印 SPDX 名称。"
        )
    return terms


def _classify_license_text(name: str, text: str, *, raw: bytes | None = None) -> set[str]:
    upper_name = name.upper()
    normalized = text.casefold()
    classes: set[str] = set()
    # Exact original bytes: dictionary source manifests and audited unicode-ident 1.0.27.
    digest = _sha256(text.encode("utf-8") if raw is None else raw)
    if digest == "0a54e647fe54104658b5e563c04c6f9edf251710e47bce692e0bd990a4ddaa39":
        classes.add("Zlib")
    if digest == "861399f8c21c042b110517e76dc6b63a2b334276c8cf17412fc3c8908ca8dc17":
        classes.add("0BSD")
    if digest == "28a9529c7d0bb4dc51f4bf5c116a3d16ef247a052f7591466768ddf563fd1cf5":
        classes.add("CC-BY-SA-4.0")
    if digest in {
        "e7a93b009565cfce55919a381437ac4db883e9da2126fa28b91d12732bc53d96",
        "f7db81051789b729fea528a63ec4c938fdcb93d9d61d97dc8cc2e9df6d47f2a1",
    }:
        classes.add("Unicode-3.0")
    if (
        "UNLICENSE" in upper_name
        or "FREE AND UNENCUMBERED SOFTWARE RELEASED INTO THE PUBLIC DOMAIN"
        in text.upper()
    ):
        classes.add("Unlicense")
    if "APACHE LICENSE" in text.upper() and "VERSION 2.0" in text.upper():
        classes.add("Apache-2.0")
    if (
        "MIT" in upper_name
        or "MIT LICENSE" in text.upper()
    ) and "PERMISSION IS HEREBY GRANTED" in text.upper():
        classes.add("MIT")
    if "COPYRIGHT" in upper_name and "permission is hereby granted" in normalized:
        classes.add("MIT")
    mit_body = (
        "Permission is hereby granted, free of charge, to any person obtaining a "
        'copy of this software and associated documentation files (the "Software"), '
        "to deal in the Software without restriction, including without limitation "
        "the rights to use, copy, modify, merge, publish, distribute, sublicense, "
        "and/or sell copies of the Software, and to permit persons to whom the "
        "Software is furnished to do so, subject to the following conditions: "
        "The above copyright notice and this permission notice shall be included "
        "in all copies or substantial portions of the Software. "
        'THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR '
        "IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, "
        "FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL "
        "THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR "
        "OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, "
        "ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR "
        "OTHER DEALINGS IN THE SOFTWARE."
    )
    if re.fullmatch(
        r"Copyright (?:©|\(c\))? ?"
        r"[0-9]{4}(?:[-, ]+[0-9]{4})*,? [^\r\n]+? "
        + re.escape(mit_body),
        " ".join(text.split()),
    ) is not None:
        classes.add("MIT")
    isc_body = (
        "Permission to use, copy, modify, and/or distribute this software for any "
        "purpose with or without fee is hereby granted, provided that the above "
        "copyright notice and this permission notice appear in all copies. "
        'THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES '
        "WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF "
        "MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR "
        "ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES "
        "WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN "
        "ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF "
        "OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE."
    )
    # 只折叠空白；正文的措辞、大小写和标点必须保持完整。
    isc_text = " ".join(text.split())
    isc_match = re.fullmatch(
        r"(?:ISC License )?Copyright (?:©|\(c\))? ?"
        r"[0-9]{4}(?:[-, ]+[0-9]{4})*,? [^\r\n]+? "
        + re.escape(isc_body),
        isc_text,
    )
    if isc_match is not None:
        classes.add("ISC")
    return classes


def _license_files_for_package(package: dict) -> list[Path]:
    manifest_value = package.get("manifest_path")
    if not isinstance(manifest_value, str) or not manifest_value:
        raise PackagingError(
            f"依赖 {package.get('name')!r} 缺少 Cargo manifest_path"
        )
    package_dir = Path(manifest_value).parent.resolve()
    if not package_dir.is_dir():
        raise PackagingError(f"依赖本地目录不存在：{package_dir}")

    candidates: set[Path] = set()
    try:
        direct_entries = list(package_dir.iterdir())
    except OSError as error:
        raise PackagingError(f"无法列出依赖许可证目录 {package_dir}：{error}") from error
    for path in direct_entries:
        if path.is_file() and path.name.upper().startswith(LICENSE_FILE_PREFIXES):
            candidates.add(path)
    licenses_dir = package_dir / "licenses"
    if licenses_dir.is_dir():
        for path in licenses_dir.rglob("*"):
            if path.is_file():
                if not path.resolve().is_relative_to(package_dir):
                    raise PackagingError("依赖许可证路径越出包目录")
                candidates.add(path)

    license_file = package.get("license_file")
    if isinstance(license_file, str) and license_file:
        declared = Path(license_file)
        if not declared.is_absolute():
            declared = package_dir / declared
        declared = declared.resolve()
        try:
            declared.relative_to(package_dir)
        except ValueError as error:
            raise PackagingError(
                f"依赖 {package.get('name')!r} 的 license_file 越出包目录"
            ) from error
        if not declared.is_file():
            raise PackagingError(
                f"依赖 {package.get('name')!r} 声明的许可证文件不存在："
                f"{declared.name}"
            )
        candidates.add(declared)

    return sorted(candidates, key=lambda path: (path.name.casefold(), path.name))


DICTIONARY_SOURCE = "git+https://github.com/axflower/flower-dict.git?branch=main#d01e9ac5f6309e05344017ebce4680cf537854a9"
DICTIONARY_NOTICE_SHA256 = "689de482c01fb308d0f04b9accc6875452070e2f645907f94f82609d35c82447"
MAX_PAYLOAD = 512 * 1024 * 1024
VOCABULARY_PREFIX = "data/pinyin/vocabulary/"
VOCABULARY_IDENTITIES = {
    "expanded.tsv": (59302671, "12669ecc1bda04813d5fdb9bd8196972ccbdca022f1b03050e217ae477043c34"),
    "catalog.tsv": (31158596, "17a0aceedb52fbf2c671073178ba91befdb21927a3309d33f90f6c0f04220e9b"),
    "coverage.json": (2900, "e9b410e6751f39d69720d67b73e9acf24962ac658e7d219db57c4fe7fc5f9b12"),
    "unresolved.txt": (225804, "c3b1dfce035f061f4a8d7c93e8c582e31f4d537fbbbbf99d79dc2acc40ff8ac0"),
}
VOCABULARY_FILES = frozenset(VOCABULARY_PREFIX + name for name in VOCABULARY_IDENTITIES)
VOCABULARY_ARCHIVE = "data/pinyin/vocabulary-audit.zip"
CONVERSION_PREFIX = "data/chinese_conversion/"
CONVERSION_IDENTITIES = {
    "STCharacters.txt": (38602, "ab3eda5e4e7c2eec9074213d5cf7acf6da93dd0ecd2a182ef716af7c8af4f949"),
    "STPhrases.txt": (1056281, "b17bbee4442b8a7f9f2678baa93baaccca5afffba14874b8cf3d129d5a6a0add"),
    "TSCharacters.txt": (39009, "9d148f979257bdf51f2bf69bf5005a5e3876d89233a8a0f525246cb540e2f6fc"),
    "TSPhrases.txt": (5710, "3b381aa65de657de807136f00d008a00697c6723ac90002170332c7b3add8ce5"),
    "manifest.json": (1037, "75b251a2f216685b3fa98fd6a11339ee0318bd7984a1ab35b32f528b1a2ac343"),
    "NOTICE.md": (804, "84cdaa9f93c1e6c354c147a59c0debf55b4080f85d46fce4d35b5690e615ad1f"),
    "LICENSE-APACHE-2.0.txt": (11558, "1eb85fc97224598dad1852b5d6483bbcf0aa8608790dcc657a5a2a761ae9c8c6"),
}
CONVERSION_FILES = frozenset(CONVERSION_PREFIX + name for name in CONVERSION_IDENTITIES)


def _verify_conversion_payload(payload: Mapping[str, bytes]) -> dict:
    if {p for p in payload if p.startswith(CONVERSION_PREFIX)} != CONVERSION_FILES:
        raise PackagingError("简繁转换文件集合不完整或包含未知路径")
    files = {}
    for name, (size, digest) in CONVERSION_IDENTITIES.items():
        path = CONVERSION_PREFIX + name
        if len(payload[path]) != size or _sha256(payload[path]) != digest:
            raise PackagingError(f"简繁转换可信长度或摘要不匹配：{path}")
        files[path] = {"size": size, "sha256": digest}
    return {"files": files, "license": "Apache-2.0"}


def _verify_vocabulary_archive(payload: Mapping[str, bytes]) -> dict:
    if VOCABULARY_ARCHIVE not in payload or any(p.startswith(VOCABULARY_PREFIX) for p in payload):
        raise PackagingError("审计 ZIP 缺失或同时携带原始词库")
    data = payload[VOCABULARY_ARCHIVE]
    limit = sum(size for size, _ in VOCABULARY_IDENTITIES.values())
    if len(data) > limit + 65536:
        raise PackagingError("审计 ZIP 超过固定上界")
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as source:
            entries = source.infolist()
            if len(entries) != len(VOCABULARY_IDENTITIES):
                raise PackagingError("审计 ZIP 成员数量不匹配")
            seen = set()
            for entry in entries:
                name = entry.filename
                if (name != entry.orig_filename or name not in VOCABULARY_IDENTITIES
                        or name in seen or entry.is_dir() or entry.flag_bits & 1
                        or entry.compress_type != zipfile.ZIP_DEFLATED
                        or stat.S_IFMT(entry.external_attr >> 16) not in (0, stat.S_IFREG)
                        or entry.external_attr & 0x400
                        or entry.file_size != VOCABULARY_IDENTITIES[name][0]):
                    raise PackagingError("审计 ZIP 成员元数据无效")
                seen.add(name)
            # Validate every central-directory record before any decompression.
            original = {}
            for entry in entries:
                size = VOCABULARY_IDENTITIES[entry.filename][0]
                with source.open(entry) as stream:
                    value = stream.read(size + 1)
                if len(value) != size:
                    raise PackagingError("审计 ZIP 成员长度无效")
                original[VOCABULARY_PREFIX + entry.filename] = value
        record = _verify_vocabulary_payload(original)
    except (OSError, ValueError, RuntimeError, zipfile.BadZipFile, EOFError, zlib.error) as error:
        raise PackagingError(f"审计 ZIP 损坏：{error}") from error
    record["representation"] = "audit-zip"
    record["archive"] = {"path": VOCABULARY_ARCHIVE, "size": len(data), "sha256": _sha256(data)}
    return record


def _vocabulary_archive_bundle(root: Path) -> tuple[dict[str, bytes], dict]:
    original, _ = _vocabulary_bundle(root)
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as target:
        for name in VOCABULARY_IDENTITIES:
            entry = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = (stat.S_IFREG | 0o644) << 16
            target.writestr(entry, original[VOCABULARY_PREFIX + name], compresslevel=9)
    payload = {VOCABULARY_ARCHIVE: buffer.getvalue()}
    return payload, _verify_vocabulary_archive(payload)


def _verify_vocabulary_payload(payload: Mapping[str, bytes]) -> dict:
    paths = {path for path in payload if path.startswith(VOCABULARY_PREFIX)}
    if paths != VOCABULARY_FILES:
        raise PackagingError("扩充词库文件集合不完整或包含未知路径")
    files = {}
    for name, (size, digest) in VOCABULARY_IDENTITIES.items():
        path = VOCABULARY_PREFIX + name
        data = payload[path]
        if len(data) != size or _sha256(data) != digest:
            raise PackagingError(f"扩充词库可信长度或摘要不匹配：{path}")
        files[path] = {"size": size, "sha256": digest}
    return {
        "files": files, "unique_entries": 2168043, "default_pinyin_pairs": 2088650,
        "automatic_pronunciations": 1887541, "unresolved_entries": 45313,
        "non_pinyin_entries": 42662, "existing_pronunciation_entries": 192527,
        "automatic_pronunciations_manually_reviewed": False,
        "license_status": "NOASSERTION", "distribution_status": "local-test-only",
    }


def _vocabulary_bundle(root: Path) -> tuple[dict[str, bytes], dict]:
    payload = {path: _read_bytes(root / path, "扩充词库") for path in sorted(VOCABULARY_FILES)}
    return payload, _verify_vocabulary_payload(payload)


def _file_license_dictionary(package: dict) -> bool:
    if package.get("license") is not None:
        return False
    if (package.get("name"), package.get("version"), package.get("source")) != (
        "flower-dict", "0.2.0", DICTIONARY_SOURCE
    ):
        return False
    root = Path(package["manifest_path"]).parent.resolve()
    declared = package.get("license_file")
    if not isinstance(declared, str) or not declared:
        raise PackagingError("字典缺少 NOTICE.md 许可声明")
    path = Path(declared)
    if not path.is_absolute():
        path = root / path
    if path.resolve() != root / "NOTICE.md":
        raise PackagingError("字典许可声明必须解析为包根 NOTICE.md")
    data = _read_bytes(path, "字典 NOTICE.md")
    if len(data) != 2370 or _sha256(data) != DICTIONARY_NOTICE_SHA256:
        raise PackagingError("字典 NOTICE.md 长度或可信摘要不匹配")
    return True


def _dependency_license_bundle(
    dependencies: Sequence[dict],
) -> tuple[dict[str, bytes], list[dict]]:
    payload: dict[str, bytes] = {}
    notices: list[dict] = []
    casefold_paths: set[str] = set()

    for package in dependencies:
        name = _safe_component(package.get("name"), "依赖名称")
        version = _safe_component(package.get("version"), "依赖版本")
        label = f"依赖 {name} {version}"
        expression = package.get("license")
        file_license = _file_license_dictionary(package)
        terms = ({"MIT", "Unicode-3.0", "CC-BY-SA-4.0"} if file_license
                 else _license_expression_terms(expression, label))
        files = _license_files_for_package(package)
        if not files:
            raise PackagingError(
                f"{label}没有可附带的本地许可证正文（LICENSE/COPYING/NOTICE 等）"
            )

        classes: set[str] = set()
        destination_dir = f"licenses/{name}-{version}"
        license_paths: list[str] = []
        for source in files:
            data = _read_bytes(source, f"{label}许可证")
            text = data.decode("utf-8", errors="replace")
            classes.update(_classify_license_text(source.name, text, raw=data))
            relative = source.relative_to(Path(package["manifest_path"]).parent.resolve())
            destination = f"{destination_dir}/{relative.as_posix()}"
            folded = destination.casefold()
            if folded in casefold_paths:
                raise PackagingError(
                    f"许可证目标路径在 Windows 上重复（大小写不敏感）：{destination}"
                )
            casefold_paths.add(folded)
            payload[destination] = data
            license_paths.append(destination)

        missing = terms - classes
        if file_license:
            root = Path(package["manifest_path"]).parent.resolve()
            for relative, digest in {
                "LICENSE-MIT": "1f048536437e3a32817a4dc0590c6ebac203d60e15fee9aa7db3df32eada2752",
                "licenses/LICENSE-pypinyin.txt": "1e6c90014b4912815c296ee64bb6f6280af47e6d4c5d80e86232dfc5defe764c",
            }.items():
                if _sha256(_read_bytes(root / relative, label)) != digest:
                    raise PackagingError(f"字典 MIT 完整正文不匹配：{relative}")
        if missing:
            names = "、".join(sorted(missing))
            raise PackagingError(
                f"{label}声明 {expression!r}，但本地许可证正文无法确认：{names}。"
                "请检查依赖是否完整下载并带有相应许可正文。"
            )

        notices.append(
            {
                "name": name,
                "version": version,
                "spdx": expression,
                **({"license_file": "NOTICE.md", "license_status": "NOASSERTION",
                    "distribution_status": "local-test-only"} if file_license else {}),
                "source": package.get("source") or "path dependency",
                "repository": package.get("repository") or "not declared",
                "license_paths": license_paths,
            }
        )

    return payload, notices


def _rust_std_bundle(sysroot: Path, rustc_version: str) -> tuple[dict[str, bytes], dict]:
    doc_root = sysroot / "share" / "doc" / "rust"
    copyright_path = doc_root / "COPYRIGHT-library.html"
    if not copyright_path.is_file():
        raise PackagingError(
            "当前 rustc sysroot 缺少 "
            f"{copyright_path}。请安装与当前 rustc 同一工具链的 rust-docs 组件"
            "（例如 rustup component add rust-docs --toolchain <当前工具链>），"
            "不能使用网络 master 文档替代。"
        )

    payload: dict[str, bytes] = {
        "licenses/rust-std/COPYRIGHT-library.html": _read_bytes(
            copyright_path, "Rust 标准库版权声明"
        )
    }
    copyright_all = doc_root / "COPYRIGHT.html"
    if copyright_all.is_file():
        payload["licenses/rust-std/COPYRIGHT.html"] = _read_bytes(
            copyright_all, "Rust 通用版权声明"
        )

    licenses_root = doc_root / "licenses"
    if not licenses_root.is_dir():
        raise PackagingError(
            f"当前 rustc sysroot 缺少许可证正文目录 {licenses_root}。"
            "请安装同工具链的 rust-docs 组件。"
        )
    license_files = sorted(
        (path for path in licenses_root.rglob("*") if path.is_file()),
        key=lambda path: path.relative_to(licenses_root).as_posix(),
    )
    if not license_files:
        raise PackagingError(f"Rust 许可证正文目录为空：{licenses_root}")
    for source in license_files:
        relative = source.relative_to(licenses_root)
        destination = f"licenses/rust-std/licenses/{relative.as_posix()}"
        payload[destination] = _read_bytes(source, "Rust 许可证正文")

    return payload, {
        "rustc": rustc_version,
        "copyright_path": "licenses/rust-std/COPYRIGHT-library.html",
        "license_paths": sorted(
            path for path in payload if path.startswith("licenses/rust-std/")
        ),
    }


def _u16(data: bytes, offset: int, description: str) -> int:
    if offset < 0 or offset + 2 > len(data):
        raise PackagingError(f"{description}越界")
    return int.from_bytes(data[offset : offset + 2], "little")


def _u32(data: bytes, offset: int, description: str) -> int:
    if offset < 0 or offset + 4 > len(data):
        raise PackagingError(f"{description}越界")
    return int.from_bytes(data[offset : offset + 4], "little")


def _checked_add(value: int, amount: int, description: str) -> int:
    result = value + amount
    if result < value:
        raise PackagingError(f"{description}偏移溢出")
    return result


def _parse_pe(data: bytes, role: str) -> PeImage:
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PackagingError(f"{role}缺少 MZ 文件头")
    pe_offset = _u32(data, 0x3C, f"{role} PE 头偏移")
    if pe_offset < 0x40 or pe_offset + 4 > len(data):
        raise PackagingError(f"{role}的 PE 头偏移无效")
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise PackagingError(f"{role}缺少 PE 签名")

    coff = pe_offset + 4
    coff_end = coff + 20
    if coff_end > len(data):
        raise PackagingError(f"{role}的 COFF 头被截断")
    machine = _u16(data, coff, f"{role}目标架构")
    if machine != 0x8664:
        raise PackagingError(f"{role}不是 AMD64（machine=0x{machine:04x}）")
    section_count = _u16(data, coff + 2, f"{role}节数量")
    if section_count == 0 or section_count > 96:
        raise PackagingError(f"{role}节数量无效：{section_count}")
    optional_size = _u16(data, coff + 16, f"{role}可选头长度")
    characteristics = _u16(data, coff + 18, f"{role}文件特性")
    is_dll = bool(characteristics & 0x2000)
    if role == "flower.dll" and not is_dll:
        raise PackagingError("flower.dll 缺少 DLL 文件标志")
    if role != "flower.dll" and is_dll:
        raise PackagingError(f"{role}不得带 DLL 文件标志")
    if optional_size < 112:
        raise PackagingError(f"{role}的 PE32+ 可选头过短")

    optional = coff_end
    optional_end = optional + optional_size
    if optional_end > len(data):
        raise PackagingError(f"{role}的可选头被截断")
    if _u16(data, optional, f"{role}可选头 magic") != 0x20B:
        raise PackagingError(f"{role}不是 PE32+")

    directory_count = _u32(data, optional + 108, f"{role}数据目录数量")
    if directory_count > 16:
        raise PackagingError(f"{role}数据目录数量异常：{directory_count}")
    required_optional_size = 112 + directory_count * 8
    if required_optional_size > optional_size:
        raise PackagingError(f"{role}的数据目录超出可选头边界")

    size_of_image = _u32(data, optional + 56, f"{role}映像大小")
    size_of_headers = _u32(data, optional + 60, f"{role}头部大小")
    if size_of_image == 0:
        raise PackagingError(f"{role}映像大小为零")
    section_table_end = optional_end + section_count * 40
    if section_table_end > len(data):
        raise PackagingError(f"{role}的节表被截断")
    if size_of_headers < section_table_end or size_of_headers > len(data):
        raise PackagingError(f"{role}的 SizeOfHeaders 无效")

    sections: list[Section] = []
    for index in range(section_count):
        offset = optional_end + index * 40
        virtual_size = _u32(data, offset + 8, f"{role}第 {index + 1} 节虚拟大小")
        virtual_address = _u32(
            data, offset + 12, f"{role}第 {index + 1} 节虚拟地址"
        )
        raw_size = _u32(data, offset + 16, f"{role}第 {index + 1} 节原始大小")
        raw_offset = _u32(data, offset + 20, f"{role}第 {index + 1} 节原始偏移")
        section = Section(virtual_address, virtual_size, raw_offset, raw_size)
        if raw_size:
            raw_end = _checked_add(raw_offset, raw_size, f"{role}节原始范围")
            if raw_offset < size_of_headers or raw_end > len(data):
                raise PackagingError(f"{role}第 {index + 1} 节原始数据越界")
        virtual_end = _checked_add(
            section.virtual_address, section.mapped_size, f"{role}节虚拟范围"
        )
        if virtual_end > size_of_image:
            raise PackagingError(f"{role}第 {index + 1} 节虚拟范围越界")
        sections.append(section)

    virtual_ranges = sorted(
        (section.virtual_address, section.virtual_address + section.mapped_size)
        for section in sections
        if section.mapped_size
    )
    for previous, current in zip(virtual_ranges, virtual_ranges[1:]):
        if current[0] < previous[1]:
            raise PackagingError(f"{role}的节虚拟地址范围重叠")

    raw_ranges = sorted(
        (section.raw_offset, section.raw_offset + section.raw_size)
        for section in sections
        if section.raw_size
    )
    for previous, current in zip(raw_ranges, raw_ranges[1:]):
        if current[0] < previous[1]:
            raise PackagingError(f"{role}的节原始数据重叠")

    export_directory = (0, 0)
    if directory_count >= 1:
        export_rva = _u32(data, optional + 112, f"{role}导出目录 RVA")
        export_size = _u32(data, optional + 116, f"{role}导出目录大小")
        if (export_rva == 0) != (export_size == 0):
            raise PackagingError(f"{role}导出目录 RVA/大小不匹配")
        export_directory = (export_rva, export_size)
        if export_rva:
            if export_size < 40:
                raise PackagingError(f"{role}导出目录过短")
            _mapped_span(
                data,
                sections,
                size_of_image,
                export_rva,
                export_size,
                f"{role}导出目录",
            )

    return PeImage(tuple(sections), size_of_image, export_directory)


def _mapped_span(
    data: bytes,
    sections: Sequence[Section],
    size_of_image: int,
    rva: int,
    size: int,
    description: str,
) -> int:
    if size < 0 or rva < 0:
        raise PackagingError(f"{description}范围无效")
    end = _checked_add(rva, size, description)
    if end > size_of_image:
        raise PackagingError(f"{description}超出映像范围")
    for section in sections:
        if rva < section.virtual_address:
            continue
        relative = rva - section.virtual_address
        if relative + size > section.mapped_size:
            continue
        if relative + size > section.raw_size:
            raise PackagingError(f"{description}未完整落在节原始数据中")
        raw = section.raw_offset + relative
        if raw + size > len(data):
            raise PackagingError(f"{description}超出文件边界")
        return raw
    raise PackagingError(f"{description}未映射到任何节")


def _rva_raw_window(
    data: bytes,
    sections: Sequence[Section],
    size_of_image: int,
    rva: int,
    description: str,
) -> tuple[int, int]:
    if rva < 0 or rva >= size_of_image:
        raise PackagingError(f"{description}超出映像范围")
    for section in sections:
        if rva < section.virtual_address:
            continue
        relative = rva - section.virtual_address
        if relative >= section.mapped_size:
            continue
        if relative >= section.raw_size:
            raise PackagingError(f"{description}不在节原始数据中")
        raw = section.raw_offset + relative
        raw_end = min(len(data), section.raw_offset + section.raw_size)
        if raw >= raw_end:
            raise PackagingError(f"{description}超出文件边界")
        return raw, raw_end
    raise PackagingError(f"{description}未映射到任何节")


def _export_names(image: PeImage, data: bytes) -> set[str]:
    export_rva, export_size = image.export_directory
    if not export_rva:
        raise PackagingError("flower.dll 没有导出目录")
    export_offset = _mapped_span(
        data,
        image.sections,
        image.size_of_image,
        export_rva,
        export_size,
        "flower.dll 导出目录",
    )
    function_count = _u32(data, export_offset + 20, "DLL 导出函数数量")
    name_count = _u32(data, export_offset + 24, "DLL 导出名称数量")
    functions_rva = _u32(data, export_offset + 28, "DLL 导出函数数组 RVA")
    names_rva = _u32(data, export_offset + 32, "DLL 导出名称数组 RVA")
    ordinals_rva = _u32(data, export_offset + 36, "DLL 导出序号数组 RVA")
    if function_count == 0 or name_count == 0:
        raise PackagingError("flower.dll 没有命名导出")
    if function_count > 0xFFFF or name_count > 0xFFFF:
        raise PackagingError("flower.dll 导出数组数量异常")

    functions_offset = _mapped_span(
        data,
        image.sections,
        image.size_of_image,
        functions_rva,
        function_count * 4,
        "DLL 导出函数数组",
    )
    names_offset = _mapped_span(
        data,
        image.sections,
        image.size_of_image,
        names_rva,
        name_count * 4,
        "DLL 导出名称数组",
    )
    ordinals_offset = _mapped_span(
        data,
        image.sections,
        image.size_of_image,
        ordinals_rva,
        name_count * 2,
        "DLL 导出序号数组",
    )
    export_end = _checked_add(export_rva, export_size, "DLL 导出目录范围")

    exported: set[str] = set()
    for index in range(name_count):
        name_rva = _u32(data, names_offset + index * 4, "DLL 导出名称 RVA")
        ordinal = _u16(data, ordinals_offset + index * 2, "DLL 导出序号")
        if ordinal >= function_count:
            raise PackagingError("flower.dll 导出序号越界")
        function_rva = _u32(
            data, functions_offset + ordinal * 4, "DLL 导出函数 RVA"
        )
        if function_rva == 0:
            raise PackagingError("flower.dll 命名导出的函数地址为空")
        if export_rva <= function_rva < export_end:
            raise PackagingError(
                f"flower.dll 导出不是本地函数，疑似 forwarder："
                f"RVA 0x{function_rva:x}"
            )
        _mapped_span(
            data,
            image.sections,
            image.size_of_image,
            function_rva,
            1,
            "DLL 导出函数地址",
        )
        name_offset, name_end = _rva_raw_window(
            data,
            image.sections,
            image.size_of_image,
            name_rva,
            "DLL 导出名称",
        )
        terminator = data.find(
            b"\0", name_offset, min(name_end, name_offset + 4096)
        )
        if terminator < 0:
            raise PackagingError("flower.dll 导出名称缺少终止符或长度异常")
        try:
            name = data[name_offset:terminator].decode("ascii")
        except UnicodeDecodeError as error:
            raise PackagingError("flower.dll 导出名称不是 ASCII") from error
        if not name:
            raise PackagingError("flower.dll 导出名称为空")
        if name in exported:
            raise PackagingError(f"flower.dll 重复导出名称：{name}")
        exported.add(name)
    return exported


def _validate_binary(path: Path, role: str) -> tuple[bytes, str]:
    if not path.is_file():
        raise PackagingError(f"{role}不存在：{path}")
    data = _read_bytes(path, role)
    image = _parse_pe(data, role)
    if role == "flower.dll":
        export_names = _export_names(image, data)
        missing = EXPECTED_DLL_EXPORTS - export_names
        if missing:
            names = "、".join(sorted(missing))
            raise PackagingError(f"flower.dll 缺少必需导出：{names}")
    return data, _sha256(data)


def _artifact_output_path(
    filename: object, target_directory: Path, role: str
) -> Path:
    if not isinstance(filename, str) or not filename:
        raise PackagingError(f"{role} artifact 缺少文件路径")
    path = Path(filename)
    if not path.is_absolute():
        path = target_directory / path
    resolved = path.resolve()
    release_directory = (target_directory / TARGET / "release").resolve()
    if resolved.parent != release_directory:
        raise PackagingError(
            f"{role} artifact 不在 {TARGET}/release 目录：{resolved}"
        )
    return resolved


def _is_release_profile(profile: object) -> bool:
    return (
        isinstance(profile, dict)
        and profile.get("opt_level") == "3"
        and profile.get("debug_assertions") is False
        and profile.get("test") is False
    )


def _select_artifacts(
    stdout: str, root_id: str, target_directory: Path
) -> ArtifactPaths:
    dll_candidates: list[Path] = []
    executable_candidates: dict[str, list[Path]] = {
        "flower-register.exe": [],
        "flower-settings.exe": [],
    }
    nonrelease_roles: set[str] = set()
    parsed_lines = 0
    for line_number, line in enumerate(stdout.splitlines(), start=1):
        if not line.strip():
            continue
        parsed_lines += 1
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            raise PackagingError(
                f"cargo build 第 {line_number} 行不是合法 JSON：{error}"
            ) from error
        if not isinstance(message, dict):
            raise PackagingError(f"cargo build 第 {line_number} 行不是对象")
        if message.get("reason") != "compiler-artifact":
            continue
        if message.get("package_id") != root_id:
            continue
        target = message.get("target")
        if not isinstance(target, dict):
            raise PackagingError("cargo build artifact 缺少 target")
        kind = target.get("kind")
        crate_types = target.get("crate_types", [])
        if not isinstance(kind, list) or not all(isinstance(item, str) for item in kind):
            raise PackagingError("cargo build artifact 的 target.kind 无效")
        if not isinstance(crate_types, list) or not all(
            isinstance(item, str) for item in crate_types
        ):
            raise PackagingError("cargo build artifact 的 target.crate_types 无效")
        target_name = target.get("name")
        filenames = message.get("filenames", [])
        if not isinstance(filenames, list):
            raise PackagingError("cargo build artifact 的 filenames 无效")

        is_cdylib = (
            target_name == "flower"
            and (
                "cdylib" in kind
                or ("lib" in kind and "cdylib" in crate_types)
            )
        )
        expected_name = {
            "flower-register": "flower-register.exe",
            "flower-settings": "flower-settings.exe",
        }.get(target_name)
        if not is_cdylib and ("bin" not in kind or expected_name is None):
            continue

        role = "flower.dll" if is_cdylib else expected_name
        profile = message.get("profile")
        if not _is_release_profile(profile):
            nonrelease_roles.add(role)
            continue

        if is_cdylib:
            dll_files = [
                filename
                for filename in filenames
                if isinstance(filename, str)
                and Path(filename).name.casefold() == "flower.dll"
            ]
            if len(dll_files) != 1:
                raise PackagingError(
                    "根包 cdylib artifact 必须恰好包含一个 flower.dll"
                )
            dll_candidates.append(
                _artifact_output_path(dll_files[0], target_directory, "flower.dll")
            )
            continue

        executable = message.get("executable")
        exe_files = [
            filename
            for filename in filenames
            if isinstance(filename, str)
            and Path(filename).name.casefold() == expected_name
        ]
        if executable is not None:
            if not isinstance(executable, str):
                raise PackagingError(f"{expected_name} artifact executable 无效")
            if Path(executable).name.casefold() != expected_name:
                raise PackagingError(
                    f"{expected_name} artifact 指向了意外可执行文件："
                    f"{Path(executable).name}"
                )
            exe_files.append(executable)
        unique_exe_files = sorted(set(exe_files))
        if len(unique_exe_files) != 1:
            raise PackagingError(
                f"根包 {target_name} artifact 必须恰好包含一个 {expected_name}"
            )
        executable_candidates[expected_name].append(
            _artifact_output_path(
                unique_exe_files[0], target_directory, expected_name
            )
        )

    if parsed_lines == 0:
        raise PackagingError("cargo build 未返回任何 JSON artifact")
    if len(dll_candidates) != 1:
        if not dll_candidates and "flower.dll" in nonrelease_roles:
            raise PackagingError(
                "cargo build 只提供了非 release flower.dll artifact；"
                "要求 opt_level=3、debug_assertions=false、test=false"
            )
        raise PackagingError("cargo build 结果缺少或有歧义的 flower.dll artifact")
    for role, candidates in executable_candidates.items():
        if len(candidates) != 1:
            if not candidates and role in nonrelease_roles:
                raise PackagingError(
                    f"cargo build 只提供了非 release {role} artifact；"
                    "要求 opt_level=3、debug_assertions=false、test=false"
                )
            raise PackagingError(
                f"cargo build 结果缺少或有歧义的 {role} artifact"
            )

    return ArtifactPaths(
        dll=dll_candidates[0],
        settings=executable_candidates["flower-settings.exe"][0],
        register=executable_candidates["flower-register.exe"][0],
    )


def _binary_records(
    artifacts: ArtifactPaths,
) -> tuple[dict[str, bytes], list[dict]]:
    binary_data: dict[str, bytes] = {}
    records: list[dict] = []
    entries = (
        ("text_service_dll", "flower.dll", artifacts.dll),
        ("settings_ui", "flower-settings.exe", artifacts.settings),
        ("registration_cli", "flower-register.exe", artifacts.register),
    )
    for role, output_name, source in entries:
        data, digest = _validate_binary(source, output_name)
        binary_data[output_name] = data
        records.append(
            {
                "role": role,
                "path": output_name,
                "size": len(data),
                "sha256": digest,
            }
        )
    return binary_data, records


def _notices_text(dependencies: Sequence[dict], rust_std: dict) -> bytes:
    lines = [
        "# Flower 第三方声明",
        "",
        "本文件按 Windows x64 解析后的 Cargo 依赖闭包生成。许可证正文原样位于",
        "`licenses/`，本文件中的 SPDX 表达式不能替代正文。",
        "",
        "## Rust 依赖包",
        "",
    ]
    if not dependencies:
        lines.extend(["（无）", ""])
    for dependency in dependencies:
        lines.extend(
            [
                f"### {dependency['name']} {dependency['version']}",
                "",
                (f"- 文件许可：`{dependency['license_file']}`；NOASSERTION；"
                 "新增数据再分发条件未核定，仅本地测试。"
                 if dependency.get("license_status") == "NOASSERTION"
                 else f"- SPDX 标识：`{dependency['spdx']}`"),
                f"- 来源：`{dependency['source']}`",
                f"- 仓库：`{dependency['repository']}`",
                "- 许可证正文：",
            ]
        )
        lines.extend(
            f"  - `{license_path}`"
            for license_path in dependency["license_paths"]
        )
        lines.append("")

    lines.extend(
        [
            "## Rust 标准库",
            "",
            f"- rustc 版本：`{rust_std['rustc']}`",
            f"- 版权声明：`{rust_std['copyright_path']}`",
            "- 许可证正文：",
        ]
    )
    lines.extend(
        f"  - `{license_path}`" for license_path in rust_std["license_paths"]
    )
    lines.append("")
    return "\n".join(lines).encode("utf-8")


def _write_payload(root: Path, payload: Mapping[str, bytes]) -> None:
    for relative, data in sorted(payload.items()):
        destination = root / Path(relative)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        destination.chmod(0o644)


def _write_zip(zip_path: Path, top_level: str, payload: Mapping[str, bytes]) -> None:
    with zipfile.ZipFile(
        zip_path,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        allowZip64=True,
    ) as archive:
        for relative, data in sorted(payload.items()):
            entry = zipfile.ZipInfo(
                filename=f"{top_level}/{relative}",
                date_time=(1980, 1, 1, 0, 0, 0),
            )
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.create_system = 3
            entry.external_attr = 0o100644 << 16
            archive.writestr(entry, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def _file_identity(path: Path, description: str) -> tuple[int, int]:
    try:
        metadata = os.stat(path, follow_symlinks=False)
    except OSError as error:
        raise PackagingError(f"无法读取{description}文件身份：{error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise PackagingError(f"{description}不是普通文件")
    return metadata.st_dev, metadata.st_ino


def _publish_no_overwrite(source: Path, destination: Path) -> PublishedFile:
    if _exists_even_dangling(destination):
        raise PackagingError(f"输出已存在，拒绝覆盖：{destination.name}")
    source_identity = _file_identity(source, "暂存产物")
    try:
        os.link(source, destination)
    except FileExistsError as error:
        raise PackagingError(f"输出已存在，拒绝覆盖：{destination.name}") from error
    except OSError as error:
        if error.errno in {
            errno.EPERM,
            errno.EXDEV,
            getattr(errno, "ENOTSUP", errno.EPERM),
            getattr(errno, "EOPNOTSUPP", errno.EPERM),
        }:
            if os.name == "nt":
                try:
                    os.rename(source, destination)
                except FileExistsError as rename_error:
                    raise PackagingError(
                        f"输出已存在，拒绝覆盖：{destination.name}"
                    ) from rename_error
                except OSError as rename_error:
                    raise PackagingError(
                        f"发布 {destination.name} 失败：{rename_error}"
                    ) from rename_error
            else:
                raise PackagingError(
                    "输出文件系统不支持原子硬链接发布；请选择支持硬链接的本地 "
                    "NTFS 目录。未发布任何产物。"
                ) from error
        else:
            raise PackagingError(f"发布 {destination.name} 失败：{error}") from error
    try:
        destination_identity = _file_identity(destination, "已发布产物")
    except PackagingError as error:
        raise PackagingError(
            f"发布后无法确认 {destination.name} 的文件身份；"
            "为避免误删，未自动回滚，可能留下该产物。"
            f"原因：{error}"
        ) from error
    if destination_identity != source_identity:
        raise PackagingError(
            f"发布后 {destination.name} 的文件身份发生变化；未将其视为本次产物"
        )
    return PublishedFile(destination, destination_identity[0], destination_identity[1])


def _dictionary_bundle(repo_root: Path) -> tuple[dict[str, bytes], dict]:
    build_lexicon = dictionary_data.load_generator(repo_root)
    """Validate the distributable data without depending on dev-only raw Unihan."""
    names = (
        "data/pinyin/base_lexicon.tsv",
        "data/pinyin/coverage.json",
        "data/pinyin/sources/manifest.json",
        "data/pinyin/sources/kMandarin_8105.txt",
    )
    payload = {name: _read_bytes(repo_root / name, "单字库及来源") for name in names}
    for name in ("UNICODE-LICENSE.txt", "LICENSE-pinyin-data.txt", "NOTICE.md"):
        payload[f"licenses/dictionaries/{name}"] = _read_bytes(
            repo_root / "data/pinyin/sources" / name, "字库许可和归属")
    try:
        coverage = json.loads(payload["data/pinyin/coverage.json"])
        sources = json.loads(payload["data/pinyin/sources/manifest.json"])
        base = payload["data/pinyin/base_lexicon.tsv"]
        if coverage.get("ranking") != build_lexicon.RANKING:
            raise ValueError("单字排序来源元数据不一致")
        # Validate only the ranking snapshot, not dev-only Unihan or CC-CEDICT.
        try:
            build_lexicon.load_frequencies(repo_root)
        except (OSError, ValueError, KeyError, TypeError) as error:
            raise ValueError(f"单字排序来源校验失败：{error}") from error
        if (coverage["schema_version"] != 1 or sources["schema_version"] != 1
                or coverage["baseline_expected"] != 8105 or coverage["baseline_covered"] != 8105
                or coverage["missing_characters"] != []
                or coverage["base_lexicon_sha256"] != _sha256(base)):
            raise ValueError("覆盖声明或生成词库哈希不一致")
        rows = [line.split("\t") for line in base.decode("utf-8").splitlines()
                if line and not line.startswith("#")]
        pairs = set()
        for row in rows:
            if (len(row) != 3 or not re.fullmatch("[a-z]{1,8}", row[0])
                    or len(row[1]) != 1 or not row[2].isascii() or not row[2].isdigit()
                    or not 0 < int(row[2]) <= 0xffffffff):
                raise ValueError("无效单字库记录")
            pairs.add((row[0], row[1]))
        characters = {char for _, char in pairs}
        baseline = []
        for line in payload["data/pinyin/sources/kMandarin_8105.txt"].decode("utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            match = re.fullmatch(r"U\+([0-9A-F]{4,6}):\s*\S+\s+#\s*(\S).*", line)
            if not match or chr(int(match[1], 16)) != match[2]:
                raise ValueError("无效 8105 检查集")
            baseline.append(match[2])
        if (len(baseline) != 8105 or len(set(baseline)) != 8105
                or not set(baseline) <= characters or len(pairs) != len(rows)
                or len(rows) != coverage["pinyin_character_pairs"]
                or len(characters) != coverage["unique_characters"]):
            raise ValueError("字库实际覆盖与报告不一致")
        for name, path in (
            ("kMandarin_8105.txt", "data/pinyin/sources/kMandarin_8105.txt"),
            ("UNICODE-LICENSE.txt", "licenses/dictionaries/UNICODE-LICENSE.txt"),
            ("LICENSE-pinyin-data.txt", "licenses/dictionaries/LICENSE-pinyin-data.txt"),
        ):
            if _sha256(payload[path]) != sources["files"][name]["sha256"]:
                raise ValueError("来源或许可正文哈希不一致")
        if (b"UNICODE LICENSE V3" not in payload["licenses/dictionaries/UNICODE-LICENSE.txt"]
                or b"The MIT License" not in payload["licenses/dictionaries/LICENSE-pinyin-data.txt"]):
            raise ValueError("字库许可正文缺失")
    except (ValueError, KeyError, TypeError, OverflowError) as error:
        raise PackagingError(f"单字库校验失败：{error}") from error
    return payload, {
        "base_lexicon_sha256": _sha256(base),
        "coverage": coverage,
        "sources": sources,
        "licenses": ["Unicode-3.0", "MIT"],
        "notice_path": "licenses/dictionaries/NOTICE.md",
    }


def _phrase_bundle(repo_root: Path) -> tuple[dict[str, bytes], dict]:
    prefix = "data/pinyin/phrases/"
    payload = {prefix + name: _read_bytes(repo_root / prefix / name, "词组数据")
               for name in ("phrase_lexicon.tsv", "proper_lexicon.tsv", "coverage.json",
                            "manifest.json", "quality-cases.json")}
    for name in ("CC-BY-SA-4.0.txt", "LICENSE-jieba.txt"):
        payload["licenses/phrases/" + name] = _read_bytes(
            repo_root / prefix / "sources" / name, "词组数据许可")
    payload["licenses/phrases/NOTICE.md"] = _read_bytes(repo_root / prefix / "NOTICE.md", "词组归属")
    try:
        manifest = json.loads(payload[prefix + "manifest.json"])
        coverage = json.loads(payload[prefix + "coverage.json"])
        if manifest["derived_data_license"] != "CC-BY-SA-4.0":
            raise ValueError("词组数据许可不一致")
        for name in ("phrase_lexicon.tsv", "proper_lexicon.tsv"):
            if _sha256(payload[prefix + name]) != coverage[name.replace(".tsv", "_sha256")]:
                raise ValueError("词组数据哈希不一致")
        for name in ("CC-BY-SA-4.0.txt", "LICENSE-jieba.txt"):
            if _sha256(payload["licenses/phrases/" + name]) != manifest["files"][name]["sha256"]:
                raise ValueError("词组许可正文哈希不一致")
        if b"Attribution-ShareAlike 4.0 International" not in payload["licenses/phrases/CC-BY-SA-4.0.txt"]:
            raise ValueError("词组许可正文缺失")
        actual = []
        for name in ("phrase_lexicon.tsv", "proper_lexicon.tsv"):
            pairs = set()
            for line in payload[prefix + name].decode("utf-8").splitlines():
                if not line or line.startswith("#"):
                    continue
                key, word, weight = line.split("\t")
                if (not re.fullmatch("[a-z]+(?:'[a-z]+)+", key)
                        or len(key.split("'")) != len(word)
                        or not 0 < int(weight) <= 0xffffffff or (key, word) in pairs):
                    raise ValueError("无效或重复词组记录")
                pairs.add((key, word))
            actual.append(pairs)
        if (actual[0] & actual[1] or len(actual[0]) != coverage["core_pairs"]
                or len(actual[1]) != coverage["optional_pairs"]):
            raise ValueError("词组分区与覆盖报告不一致")
    except (KeyError, TypeError, ValueError) as error:
        raise PackagingError(f"词组数据校验失败：{error}") from error
    return payload, {"license": "CC-BY-SA-4.0", "sources": manifest,
                     "coverage": coverage, "notice_path": "licenses/phrases/NOTICE.md"}


EVERYDAY_COUNT = 803
CATEGORY_PREFIX = "data/pinyin/categories/"
# Updating the offline snapshot requires an explicit code and evidence update.
CATEGORY_MANIFEST_SHA256 = "ae910264a79644ed146338169411e319c01053be0c5a9dbdbf886fc8e2d95603"
CATEGORY_TABLES = tuple(name + ".tsv" for name in (
    "IT", "animal", "caijing", "car", "chengyu", "diming", "food", "law",
    "lishimingren", "medical", "poem", "cedict_supplement", "erhua",
))
CATEGORY_LICENSE_SOURCES = {
    "data/pinyin/sources/thuocl/LICENSE": ("thuocl", "LICENSE"),
    "data/pinyin/sources/thuocl/README.md": ("thuocl", "README.md"),
    "data/pinyin/phrases/sources/CC-BY-SA-4.0.txt": ("phrases", "CC-BY-SA-4.0.txt"),
    "data/pinyin/phrases/sources/LICENSE-jieba.txt": ("phrases", "LICENSE-jieba.txt"),
    "data/pinyin/sources/UNICODE-LICENSE.txt": ("characters", "UNICODE-LICENSE.txt"),
    "data/pinyin/sources/LICENSE-pinyin-data.txt": ("characters", "LICENSE-pinyin-data.txt"),
}
CATEGORY_FILES = frozenset(
    [CATEGORY_PREFIX + name for name in (*CATEGORY_TABLES, "manifest.json", "coverage.json", "NOTICE.md")]
    + list(CATEGORY_LICENSE_SOURCES)
)


def _verify_category_payload(
    payload: Mapping[str, bytes], repo_root: Path, *, dictionary_root: Path | None = None,
) -> dict:
    if dictionary_root is None:
        dictionary_root = dictionary_data.resolve_dictionary_root(repo_root)
    """Verify generated records and independently pinned source-license bytes."""
    try:
        manifest_bytes = payload[CATEGORY_PREFIX + "manifest.json"]
        if _sha256(manifest_bytes) != CATEGORY_MANIFEST_SHA256:
            raise ValueError("分类 manifest 不匹配代码内可信快照摘要")
        manifest = json.loads(manifest_bytes)
        base_data = payload["data/pinyin/base_lexicon.tsv"]
        base_info = manifest["baseline"]["data/pinyin/base_lexicon.tsv"]
        if len(base_data) != base_info["bytes"] or _sha256(base_data) != base_info["sha256"]:
            raise ValueError("分类 base 音节来源摘要不一致")
        syllables = {
            line.split("\t", 1)[0] for line in base_data.decode("utf-8").splitlines()
            if line and not line.startswith("#")
        } | {"r", "sei"}
        coverage = json.loads(payload[CATEGORY_PREFIX + "coverage.json"])
        if manifest["schema_version"] != 1 or manifest["default_loaded"] is not True:
            raise ValueError("分类 schema/default_loaded 无效")
        expected = set(CATEGORY_TABLES) | {"coverage.json", "NOTICE.md", "provenance.tsv", "rejected.tsv"}
        if set(manifest["files"]) != expected:
            raise ValueError("分类文件清单不是固定白名单")
        source_paths = {
            "thuocl": "data/pinyin/sources/thuocl/manifest.json",
            "phrases": "data/pinyin/phrases/manifest.json",
            "characters": "data/pinyin/sources/manifest.json",
        }
        for source, path in source_paths.items():
            trusted = json.loads(_read_bytes(dictionary_root / path, "分类来源清单"))
            if manifest["sources"][source] != trusted:
                raise ValueError("分类来源清单与固定源不一致")
        for path, (source, name) in CATEGORY_LICENSE_SOURCES.items():
            info = manifest["sources"][source]["files"][name]
            data = payload[path]
            if _sha256(data) != info["sha256"] or (
                    "bytes" in info and len(data) != info["bytes"]):
                raise ValueError("分类许可字节不一致：" + path)
        union = set()
        for name in (*CATEGORY_TABLES, "coverage.json", "NOTICE.md"):
            data = payload[CATEGORY_PREFIX + name]
            info = manifest["files"][name]
            lines = data.decode("utf-8").splitlines()
            if (len(data) != info["bytes"] or _sha256(data) != info["sha256"]
                    or len(lines) != info["lines"]):
                raise ValueError("分类文件摘要/字节/行数不一致：" + name)
            if name not in CATEGORY_TABLES:
                continue
            pairs = set()
            for line in lines:
                reading, text, weight = line.split("\t")
                if (not re.fullmatch("[a-z]+(?:'[a-z]+)+", reading)
                        or not re.fullmatch("[\u4e00-\u9fff]{2,16}", text)
                        or len(reading.split("'")) != len(text)
                        or any(part not in syllables for part in reading.split("'"))
                        or not re.fullmatch("[1-9][0-9]*", weight)
                        or int(weight) > 0xffffffff or (reading, text) in pairs):
                    raise ValueError("不规范或重复分类记录：" + name)
                pairs.add((reading, text))
            if len(pairs) != info["pairs"]:
                raise ValueError("分类记录数不一致：" + name)
            union.update(pairs)
        if len(union) != manifest["global_unique_pairs"] or len(union) != coverage["global_unique_pairs"]:
            raise ValueError("分类全局唯一词对不一致")
        baseline = set()
        for path, info in manifest["baseline"].items():
            if path not in {"data/pinyin/base_lexicon.tsv", "data/pinyin/everyday_phrases.tsv",
                            "data/pinyin/phrases/phrase_lexicon.tsv"}:
                raise ValueError("分类 baseline 路径无效")
            data = payload[path]
            if len(data) != info["bytes"] or _sha256(data) != info["sha256"]:
                raise ValueError("分类 baseline 摘要不一致")
            baseline.update(tuple(line.split("\t")[:2]) for line in data.decode("utf-8").splitlines()
                            if line and not line.startswith("#"))
        if (len(manifest["baseline"]) != 3
                or len(union - baseline) != manifest["additional_to_default"]
                or manifest["additional_to_default"] != coverage["additional_to_default"]):
            raise ValueError("分类默认增量不一致")
    except (KeyError, TypeError, ValueError, UnicodeError) as error:
        raise PackagingError(f"分类词库校验失败：{error}") from error
    return {
        "paths": sorted(CATEGORY_FILES), "default_loaded": True,
        "global_unique_pairs": len(union), "additional_to_default": len(union - baseline),
        "files": {path: {"bytes": len(payload[path]), "sha256": _sha256(payload[path])}
                  for path in sorted(CATEGORY_FILES)},
        "sources": manifest["sources"], "coverage": coverage,
        "licenses": list(CATEGORY_LICENSE_SOURCES),
        "weight_kind": "bounded ranking values / existing jieba weights; not global corpus frequency",
    }


def _category_bundle(repo_root: Path) -> tuple[dict[str, bytes], dict]:
    payload = {path: _read_bytes(repo_root / path, "分类运行数据") for path in CATEGORY_FILES}
    baseline = {path: _read_bytes(repo_root / path, "分类默认基线") for path in (
        "data/pinyin/base_lexicon.tsv", "data/pinyin/everyday_phrases.tsv",
        "data/pinyin/phrases/phrase_lexicon.tsv")}
    return payload, _verify_category_payload(
        {**payload, **baseline}, repo_root, dictionary_root=repo_root,
    )


EVERYDAY_SHA256 = "0b61378faaecc1a53f71400b402d1f3af3ae65b7be2b5b545b2785290fc8fdae"


def _everyday_bundle(repo_root: Path) -> tuple[dict[str, bytes], dict]:
    path = "data/pinyin/everyday_phrases.tsv"
    notice = "data/pinyin/everyday_phrases.md"
    payload = {name: _read_bytes(repo_root / name, "项目人工短语")
               for name in (path, notice)}
    try:
        pairs = set()
        for line in payload[path].decode("utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            key, word, weight = line.split("\t")
            if (not re.fullmatch("[a-z]+(?:'[a-z]+)+", key)
                    or len(key.split("'")) != len(word)
                    or any(ord(c) < 32 for c in word)
                    or int(weight) != 100 or (key, word) in pairs):
                raise ValueError("无效或重复人工短语")
            pairs.add((key, word))
        if len(pairs) != EVERYDAY_COUNT or _sha256(payload[path]) != EVERYDAY_SHA256:
            raise ValueError("人工短语数量或哈希不一致")
        text = payload[notice].decode("utf-8")
        if ("MIT" not in text or "人工" not in text or "不是统计频率" not in text
                or EVERYDAY_SHA256 not in text or str(EVERYDAY_COUNT) not in text):
            raise ValueError("人工短语来源许可说明不一致")
    except (UnicodeError, ValueError) as error:
        raise PackagingError(f"项目人工短语校验失败：{error}") from error
    return payload, {
        "path": path, "pairs": len(pairs), "sha256": _sha256(payload[path]),
        "origin": "Flower contributors; project-authored",
        "license": "MIT", "license_path": "LICENSE-MIT", "notice_path": notice,
        "weight_kind": "manual, not corpus frequency",
    }


def _build_payloads(
    repo_root: Path,
    version: str,
    binary_data: Mapping[str, bytes],
    binary_records: Sequence[dict],
    dependency_payload: Mapping[str, bytes],
    dependency_notices: Sequence[dict],
    rust_payload: Mapping[str, bytes],
    rust_std: dict,
    ai_bundle: tuple[dict[str, bytes], dict] | None = None,
    dictionary_root: Path | None = None,
) -> tuple[dict[str, bytes], str]:
    if dictionary_root is None:
        dictionary_root = dictionary_data.resolve_dictionary_root(repo_root)
    top_level, _, _ = _package_paths(version)
    readme = _read_bytes(repo_root / "docs" / "distribution.md", "中文分发说明")
    lexicon = _read_bytes(
        dictionary_root / "data" / "pinyin" / "mvp_lexicon.tsv", "MVP 词库"
    )
    dictionary_payload, dictionary_record = _dictionary_bundle(dictionary_root)
    phrase_payload, phrase_record = _phrase_bundle(dictionary_root)
    everyday_payload, everyday_record = _everyday_bundle(dictionary_root)
    category_payload, category_record = _category_bundle(dictionary_root)
    vocabulary_payload = {}
    vocabulary_record = None
    release = tuple(int(part) for part in re.split(r"[-+]", version, maxsplit=1)[0].split("."))
    conversion_payload = {}
    conversion_record = None
    if release >= (0, 4, 22):
        vocabulary_payload, vocabulary_record = _vocabulary_archive_bundle(dictionary_root)
        conversion_payload = {p: _read_bytes(repo_root / p, "简繁转换数据") for p in sorted(CONVERSION_FILES)}
        conversion_record = _verify_conversion_payload(conversion_payload)
    elif release >= (0, 4, 21):
        vocabulary_payload, vocabulary_record = _vocabulary_bundle(dictionary_root)
    cargo_lock = _read_bytes(repo_root / "Cargo.lock", "Cargo.lock")
    root_mit = _read_bytes(repo_root / "LICENSE-MIT", "MIT 许可证")
    root_apache = _read_bytes(repo_root / "LICENSE-APACHE", "Apache 许可证")
    root_terms = _license_expression_terms(EXPECTED_ROOT_LICENSE, "根包")
    root_classes = _classify_license_text(
        "LICENSE-MIT", root_mit.decode("utf-8", errors="replace")
    )
    root_classes.update(
        _classify_license_text(
            "LICENSE-APACHE", root_apache.decode("utf-8", errors="replace")
        )
    )
    missing_root_terms = root_terms - root_classes
    if missing_root_terms:
        names = "、".join(sorted(missing_root_terms))
        raise PackagingError(
            f"根包许可声明 {EXPECTED_ROOT_LICENSE!r} 缺少可确认正文：{names}"
        )

    payload: dict[str, bytes] = {
        "flower.dll": binary_data["flower.dll"],
        "flower-settings.exe": binary_data["flower-settings.exe"],
        "flower-register.exe": binary_data["flower-register.exe"],
        "README.md": readme,
        "LICENSE-MIT": root_mit,
        "LICENSE-APACHE": root_apache,
        "data/pinyin/mvp_lexicon.tsv": lexicon,
        "Cargo.lock": cargo_lock,
    }
    for source in (dependency_payload, rust_payload, dictionary_payload, phrase_payload,
                   everyday_payload, category_payload, vocabulary_payload, conversion_payload):
        for relative, data in source.items():
            if relative in payload:
                raise PackagingError(f"分发文件路径重复：{relative}")
            payload[relative] = data

    notices = _notices_text(dependency_notices, rust_std) + (
        "\n## 汉字读音数据\n\nUnicode-3.0 与 MIT 许可；固定来源、覆盖报告及完整许可见\n"
        "`data/pinyin/sources/manifest.json` 和 `licenses/dictionaries/NOTICE.md`。\n"
    ).encode("utf-8")
    payload["THIRD-PARTY-NOTICES.md"] = notices
    payload["THIRD-PARTY-NOTICES.md"] += (
        "\n## 词组数据\n\n词组表按独立的 CC BY-SA 4.0 分发，词频另保留 jieba MIT 许可。\n"
        "归属、修改说明、相同方式共享条件和完整许可见 `licenses/phrases/NOTICE.md`。\n"
        "数据许可不改写 Flower 程序代码的许可。\n"
    ).encode("utf-8")

    if ai_bundle is not None:
        payload.update(ai_bundle[0])
    manifest = {
        "schema_version": 1,
        "package": "flower",
        "version": version,
        "variant": "ai" if ai_bundle is not None else "base",
        "ai": ai_bundle[1] if ai_bundle is not None else {"available": False},
        "target": TARGET,
        "rustc": rust_std["rustc"],
        "cargo_lock_sha256": _sha256(cargo_lock),
        "binaries": list(binary_records),
        "third_party": list(dependency_notices),
        "rust_standard_library": rust_std,
        "dictionaries": dictionary_record,
        "phrases": phrase_record,
        "everyday_phrases": everyday_record,
        "category_lexicons": category_record,
        "distribution": {
            "format": "zip",
            "authenticode_signed": False,
            "windows_desktop_verified": False,
        },
    }
    if vocabulary_record is not None:
        manifest["expanded_vocabulary"] = vocabulary_record
    if conversion_record is not None:
        manifest["chinese_conversion"] = conversion_record
    manifest_bytes = (
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")
    payload["manifest.json"] = manifest_bytes

    checksum_lines = []
    for relative, data in sorted(payload.items()):
        checksum_lines.append(f"{_sha256(data)}  {relative}")
    payload["SHA256SUMS"] = ("\n".join(checksum_lines) + "\n").encode("utf-8")
    if any(len(data) > MAX_PAYLOAD for data in payload.values()) or sum(map(len, payload.values())) > MAX_PAYLOAD:
        raise PackagingError("包内 payload 超过 512 MiB")
    return payload, top_level


def _rollback_published(published: Sequence[PublishedFile]) -> list[str]:
    issues: list[str] = []
    for item in reversed(published):
        try:
            if not _exists_even_dangling(item.path):
                continue
            current_identity = _file_identity(item.path, "已发布产物")
            if current_identity != (item.device, item.inode):
                issues.append(
                    f"{item.path.name} 已被外部替换，未删除外部文件"
                )
                continue
            item.path.unlink()
        except PackagingError as error:
            issues.append(f"{item.path.name} 回滚失败：{error}")
        except OSError as error:
            issues.append(f"{item.path.name} 回滚失败：{error}")
    return issues


def _cleanup_staging(staging: Path | None) -> None:
    if staging is None:
        return
    try:
        if not _exists_even_dangling(staging):
            return
        if staging.is_symlink() or not staging.is_dir():
            raise PackagingError(
                f"临时 staging 路径已不是本次目录，拒绝清理：{staging}"
            )
        shutil.rmtree(staging)
    except PackagingError:
        raise
    except OSError as error:
        raise PackagingError(f"清理临时 staging 失败：{staging}：{error}") from error


def build_package(
    *,
    repo_root: Path | None = None,
    output_dir: Path | None = None,
    host_platform: str | None = None,
    runner: Runner | None = None,
    ai_assets: Path | None = None,
) -> PackageResult:
    """执行真实校验并构建分发包。

    `host_platform` 和 `runner` 仅用于测试内部入口；命令行的实际构建没有跳过
    校验或使用 fixture PE 的选项。
    """

    repo = (
        Path(__file__).resolve().parent.parent
        if repo_root is None
        else Path(repo_root).resolve()
    )
    output = (
        repo / "dist"
        if output_dir is None
        else Path(output_dir).expanduser().resolve()
    )
    platform_name = sys.platform if host_platform is None else host_platform
    if platform_name != "win32":
        raise PackagingError(
            f"实际 ZIP 打包只支持 Windows；当前平台为 {platform_name!r}。"
            "请在有 MSVC x64 工具链的 Windows 10/11 上运行。"
        )
    invoke = _subprocess_runner if runner is None else runner
    if ai_assets is None:
        raise PackagingError("必须显式提供 --ai-assets AI 资产目录；不再构建普通包")
    ai_bundle = None
    if ai_assets is not None:
        try:
            ai_bundle = package_ai_assets.load_bundle(Path(ai_assets), repo)
        except (OSError, ValueError, TypeError, KeyError) as error:
            raise PackagingError(f"AI 资产校验失败：{error}") from error

    metadata_stdout = _checked_output(
        invoke,
        [
            "cargo",
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            TARGET,
        ],
        repo,
        "cargo metadata",
    )
    metadata, root_package, package_by_id = _load_metadata(metadata_stdout)
    try:
        dictionary_root = dictionary_data.resolve_dictionary_root(repo, metadata)
    except ValueError as error:
        raise PackagingError(str(error)) from error
    root_id = metadata["resolve"]["root"]
    version = _safe_component(root_package.get("version"), "包版本")
    _, zip_name, checksum_name = _package_paths(version)
    zip_path = output / zip_name
    checksum_path = output / checksum_name
    _reject_existing_outputs(zip_path, checksum_path)

    target_directory_value = metadata.get("target_directory")
    if not isinstance(target_directory_value, str) or not target_directory_value:
        raise PackagingError("cargo metadata 缺少 target_directory")
    target_directory = Path(target_directory_value).resolve()

    build_stdout = _checked_output(
        invoke,
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "--lib",
            "--bin",
            "flower-register",
            "--bin",
            "flower-settings",
            "--target",
            TARGET,
            "--message-format=json-render-diagnostics",
        ],
        repo,
        "cargo build",
    )
    artifacts = _select_artifacts(build_stdout, root_id, target_directory)
    binary_data, binary_records = _binary_records(artifacts)

    rustc_version = _checked_output(
        invoke, ["rustc", "-Vv"], repo, "获取 rustc 版本"
    ).strip()
    if not rustc_version:
        raise PackagingError("rustc -Vv 未返回版本信息")
    sysroot_text = _checked_output(
        invoke, ["rustc", "--print", "sysroot"], repo, "获取 rustc sysroot"
    ).strip()
    if not sysroot_text:
        raise PackagingError("rustc --print sysroot 未返回路径")
    sysroot = Path(sysroot_text)
    if not sysroot.is_dir():
        raise PackagingError(f"rustc sysroot 目录不存在：{sysroot}")

    dependencies = _resolved_dependencies(metadata, root_id, package_by_id)
    dependency_payload, dependency_notices = _dependency_license_bundle(dependencies)
    rust_payload, rust_std = _rust_std_bundle(sysroot, rustc_version)
    payload, top_level = _build_payloads(
        repo,
        version,
        binary_data,
        binary_records,
        dependency_payload,
        dependency_notices,
        rust_payload,
        rust_std,
        ai_bundle,
        dictionary_root=dictionary_root,
    )

    if _exists_even_dangling(output):
        if not output.is_dir() or output.is_symlink():
            raise PackagingError(f"--output-dir 不是普通目录：{output}")
    else:
        try:
            output.mkdir(parents=True)
        except OSError as error:
            raise PackagingError(f"无法创建输出目录 {output}：{error}") from error

    staging: Path | None = None
    published: list[PublishedFile] = []
    try:
        staging = Path(tempfile.mkdtemp(prefix=".flower-package-", dir=output))
        stage_root = staging / top_level
        stage_root.mkdir()
        _write_payload(stage_root, payload)

        staged_zip = staging / zip_name
        _write_zip(staged_zip, top_level, payload)
        digest = hashlib.sha256()
        with staged_zip.open("rb") as archive:
            for chunk in iter(lambda: archive.read(1024 * 1024), b""):
                digest.update(chunk)
        staged_checksum = staging / checksum_name
        staged_checksum.write_text(
            f"{digest.hexdigest()}  {zip_name}\n",
            encoding="ascii",
            newline="\n",
        )

        _reject_existing_outputs(zip_path, checksum_path)
        published.append(_publish_no_overwrite(staged_zip, zip_path))
        published.append(_publish_no_overwrite(staged_checksum, checksum_path))
        _cleanup_staging(staging)
        staging = None
    except BaseException as original_error:
        cleanup_issues = _rollback_published(published)
        try:
            _cleanup_staging(staging)
        except PackagingError as error:
            cleanup_issues.append(str(error))
        if cleanup_issues:
            detail = "；".join(cleanup_issues)
            combined = PackagingError(
                "打包失败，且未完成全部回滚或清理。"
                f"原始错误：{type(original_error).__name__}: {original_error}。"
                f"残留与清理问题：{detail}"
            )
            if isinstance(original_error, Exception):
                raise combined from original_error
            raise BaseExceptionGroup(
                "打包失败且清理未完成",
                [original_error, combined],
            )
        raise
    return PackageResult(zip_path, checksum_path, version)


def _build_parser() -> argparse.ArgumentParser:
    parser = _ChineseArgumentParser(
        usage="%(prog)s [选项]",
        add_help=False,
        formatter_class=_ChineseHelpFormatter,
        description=(
            "构建 Flower Windows 10/11 x64 ZIP 分发包。工具会执行锁定依赖的 "
            "Cargo 构建，严格校验 DLL/EXE、许可证与 Rust 标准库文档，并生成"
            "固定顺序和时间的 ZIP 与外部 SHA-256。"
        ),
        epilog=(
            "默认输出到仓库根目录的 dist/；已有 zip 或 .zip.sha256 时拒绝覆盖。"
            "实际打包仅支持 Windows。"
        ),
    )
    parser._optionals.title = "选项"
    parser.add_argument(
        "--ai-assets", type=Path, metavar="目录", required=True,
        help="显式指定固定九文件 AI 资产目录，构建完整离线分发包",
    )
    parser.add_argument(
        "-h",
        "--help",
        action="help",
        help="显示此帮助并退出",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        metavar="目录",
        help="ZIP 与 .zip.sha256 的输出目录（默认：仓库根目录/dist）",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _build_parser()
    args = parser.parse_args(argv)
    try:
        result = build_package(output_dir=args.output_dir, ai_assets=args.ai_assets)
    except PackagingError as error:
        print(f"错误：{error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"错误：打包文件操作失败：{error}", file=sys.stderr)
        return 1
    print(f"已生成：{result.zip_path}")
    print(f"校验值：{result.checksum_path}")
    print("注意：这是未签名 ZIP，尚未完成 Windows 桌面验收。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
