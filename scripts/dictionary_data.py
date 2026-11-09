"""Locate dictionary assets only through Cargo's resolved direct dependency."""

from functools import lru_cache
import importlib.util
import json
from pathlib import Path
import subprocess
import sys


def resolve_dictionary_root(repo_root: Path, metadata=None) -> Path:
    repo_root = Path(repo_root).resolve()
    if metadata is None:
        result = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--offline"],
            cwd=repo_root, check=True, capture_output=True, text=True,
        )
        metadata = json.loads(result.stdout)
    try:
        packages = metadata["packages"]
        by_id = {package["id"]: package for package in packages}
        if len(by_id) != len(packages):
            raise ValueError("重复 package ID")
        resolve = metadata["resolve"]
        root_id = resolve.get("root")
        if root_id is None:
            roots = [p["id"] for p in packages
                     if p["name"] == "flower"
                     and Path(p["manifest_path"]).resolve() == repo_root / "Cargo.toml"]
            if len(roots) != 1:
                raise ValueError("无法唯一确定 Flower 根包")
            root_id = roots[0]
        if by_id[root_id]["name"] != "flower":
            raise ValueError("根包不是 Flower")
        root_manifest = Path(by_id[root_id]["manifest_path"])
        if (not root_manifest.is_absolute()
                or root_manifest.resolve() != repo_root / "Cargo.toml"):
            raise ValueError("Flower 根 manifest_path 与请求仓库不一致")
        nodes = [node for node in resolve["nodes"] if node["id"] == root_id]
        if len(nodes) != 1:
            raise ValueError("Flower resolve 节点缺失或重复")
        # Resolve by package ID, not the dependency's possibly renamed crate name.
        matches = [by_id[dep["pkg"]] for dep in nodes[0]["deps"]
                   if by_id[dep["pkg"]]["name"] == "flower-dictionary"]
        if len(matches) != 1 or matches[0]["version"] != "0.1.0":
            raise ValueError("必须唯一直接依赖 flower-dictionary 0.1.0")
        manifest = Path(matches[0]["manifest_path"])
        if not manifest.is_absolute() or manifest.name != "Cargo.toml":
            raise ValueError("字典 manifest_path 异常")
        manifest = manifest.resolve(strict=True)
        if not manifest.is_file():
            raise ValueError("字典 manifest_path 不是文件")
        return manifest.parent
    except (KeyError, TypeError, OSError) as error:
        raise ValueError(f"无效字典依赖 metadata：{error}") from error


@lru_cache(maxsize=None)
def load_generator(root: Path):
    source = Path(root).resolve() / "scripts" / "build_lexicon.py"
    spec = importlib.util.spec_from_file_location(
        f"_flower_dictionary_generator_{len(_GENERATORS)}", source
    )
    if spec is None or spec.loader is None:
        raise ValueError(f"无法加载字典生成器：{source}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(spec.name, None)
        raise
    _GENERATORS.append(module)
    return module


_GENERATORS = []
