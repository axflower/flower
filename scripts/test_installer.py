"""安装器构建边界测试；使用合成 PE 和注入 runner，不执行安装器。"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from scripts import installer
from scripts.test_package import PackageFixture, ai_fixture, flower_package, make_pe


class InstallerSourceTests(unittest.TestCase):
    def test_ui_element_capability_is_required_and_orphans_are_detected(self):
        source = (installer.REPO / "installer" / "flower.iss").read_text(encoding="utf-8-sig")
        self.assertIn("UIElementCategory = '{49D2F9CF-1F5E-11D7-A6D3-00065B84435C}'", source)
        # Both initial conflicts and post-registration ownership cover the capability.
        self.assertGreaterEqual(source.count("UIElementCategory + '\\' + FlowerClass"), 2)
        owned = source.split("function OwnedRegistrationComplete", 1)[1].split("function CanStartFlower", 1)[0]
        self.assertIn("KeyboardCategory", owned)
        self.assertIn("UIElementCategory", owned)

    """Static integration guards, not a substitute for executing the wizard."""

    def test_shortcuts_have_clean_names_without_changing_commands(self):
        source = (installer.REPO / "installer/flower.iss").read_text(encoding="utf-8-sig")
        icons = source.split("[Icons]", 1)[1].split("[Run]", 1)[0]
        self.assertIn(r'Name: "{commonprograms}\Flower\启动 Flower";', icons)
        self.assertIn(r'Name: "{commonprograms}\Flower\停用 Flower";', icons)
        self.assertNotIn("当前用户", icons)
        self.assertNotIn("(", icons)
        self.assertIn('Parameters: "enable"', icons)
        self.assertIn('Parameters: "disable"', icons)
        self.assertNotIn("runas", icons)

    def test_non_elevated_copy_and_handle_only_rollback_contract(self):
        source = (installer.REPO / "src/platform/install_path.rs").read_text(encoding="utf-8-sig")
        copy = source.split("pub fn install_user_files", 1)[1].split("#[cfg(test)]", 1)[0]
        self.assertLess(copy.index("require_non_elevated()?"), copy.index("user_path_guards(path, true)"))
        self.assertIn("elevation.TokenIsElevated != 0", source)
        self.assertIn(".create_new(true)", copy)
        self.assertIn(".share_mode(0)", copy)
        self.assertIn("SetFileInformationByHandle", copy)
        self.assertNotIn("remove_file", copy)
        self.assertNotIn("remove_dir_all", copy)
        self.assertNotIn("fs::copy", copy)
        self.assertIn("user_directory_guard(directory)", source)

    def test_fixed_directory_and_pre_copy_path_gate(self):
        source = (installer.REPO / "installer/flower.iss").read_text(encoding="utf-8-sig")
        self.assertIn("DisableDirPage=yes", source)
        self.assertIn(r"DefaultDirName={commonpf64}\Flower", source)
        self.assertIn("UsePreviousAppDir=no", source)
        self.assertIn(r"UninstallFilesDir={app}", source)
        self.assertIn(r"{app}\versions\{#BuildId}", source)
        self.assertNotIn("CreateInputDirPage", source)
        self.assertNotIn("SystemRoot", source)
        self.assertIn("/DIR", source)
        self.assertIn("AllowUNCPath=no", source)
        self.assertIn("Flags: dontcopy", source)
        prepare = source.split("function PrepareToInstall", 1)[1].split("procedure CurStepChanged", 1)[0]
        self.assertIn(r"SafeInstallPath(ExpandConstant('{app}'))", prepare)
        self.assertIn("PrepareInstallPath(WizardDirValue)", prepare)
        self.assertLess(prepare.index("if not FixedInstallRoot"), prepare.index("PrepareInstallPath("))
        self.assertIn("ExtractTemporaryFile('flower-path-check.exe')", prepare)
        checker = source.split("function SafeInstallPath", 1)[1].split("function FixedInstallRoot", 1)[0]
        self.assertLess(checker.index("GetSHA256OfFile"), checker.index("if Exec("))
        self.assertIn("check-install-path --path", checker)
        fixed = source.split("function FixedInstallRoot", 1)[1].split("function PrepareInstallPath", 1)[0]
        self.assertIn("CompareStringOrdinal", fixed)
        self.assertIn("ExpandFileName", fixed)
        self.assertIn(r"{commonpf64}\Flower", fixed)
        self.assertNotIn("check-user-install-path", source)
        for gate in ["HasFlowerRegistration", "DirExists(PayloadDir)", "HasNativeRuntime"]:
            self.assertLess(prepare.index(gate), prepare.index("PrepareInstallPath("))
        self.assertLess(prepare.index("PrepareInstallPath("), prepare.index("SafeInstallPath("))
        creator = source.split("function PrepareInstallPath", 1)[1].split("function HasSubkeys", 1)[0]
        self.assertLess(creator.index("GetSHA256OfFile"), creator.index("if Exec("))
        self.assertIn("prepare-install-path --path", creator)

    def test_default_checked_start_is_original_user_and_fail_closed(self):
        source = (installer.REPO / "installer/flower.iss").read_text(encoding="utf-8-sig")
        run = source.split("[Run]", 1)[1].split("[Code]", 1)[0]
        self.assertIn('Parameters: "--start-input-method"', run)
        self.assertIn('Description: "启动 Flower"', run)
        self.assertIn("postinstall nowait skipifsilent runasoriginaluser", run)
        immediate, startup = run.strip().splitlines()
        self.assertNotIn("unchecked", immediate)
        self.assertIn('Parameters: "--enable-startup"', startup)
        self.assertIn('Description: "开机自启动"', startup)
        self.assertIn("postinstall unchecked skipifsilent runasoriginaluser", startup)
        self.assertNotIn("nowait", startup)
        self.assertIn("Check: CanStartFlower", startup)
        self.assertIn("Check: CanStartFlower", run)
        gate = source.split("function CanStartFlower", 1)[1].split("function InitializeSetup", 1)[0]
        for check in ["RegistrationFailed", "FixedInstallRoot", "SafeInstallPath",
                      "TrustedPayload", "OwnedRegistrationComplete", "ExpectedSettingsHash"]:
            self.assertIn(check, gate)
        self.assertIn("RegistrationFailed := True", source.split("function InitializeSetup", 1)[1])
        cli = (installer.REPO / "src/bin/flower-settings.rs").read_text(encoding="utf-8-sig")
        self.assertIn('[arg] if arg == "--start-input-method"', cli)
        start = cli.split("fn start_input_method()", 1)[1].split("fn execute(", 1)[0]
        self.assertLess(start.index("require_non_elevated()?"), start.index("registration::enable()"))
        self.assertIn("registration::enable()", start)
        self.assertNotIn("run_settings_app", start)
        self.assertIn("TokenIsElevated != 0", cli)
        self.assertIn('command(&["--start-input-method", "extra"]).is_err()', cli)
        self.assertIn('cfg(not(target_os = "windows"))', start)

    def test_startup_is_user_owned_silent_and_never_cleaned_by_uninstaller(self):
        source = (installer.REPO / "installer/flower.iss").read_text(encoding="utf-8-sig")
        icons = source.split("[Icons]", 1)[1].split("[Run]", 1)[0]
        self.assertIn(r'\关闭开机自启动";', icons)
        self.assertIn('Parameters: "--disable-startup"', icons)
        self.assertNotIn("runas", icons)
        self.assertNotIn("启用并打开设置", source)
        uninstall = source.split("function InitializeUninstall", 1)[1]
        self.assertIn("先运行“关闭开机自启动”再“停用 Flower”", uninstall)
        for forbidden in ["HKEY_USERS", "HKU", "RegLoadKey", "RegDeleteValue",
                          "allusersstartup", "schtasks", "[UninstallRun]"]:
            self.assertNotIn(forbidden, source)
        cli = (installer.REPO / "src/bin/flower-settings.rs").read_text(encoding="utf-8-sig")
        dispatch = cli.split("fn execute(", 1)[1].split("Command::AiServer", 1)[0]
        self.assertLess(dispatch.index("require_non_elevated()?"),
                        dispatch.index("validate_current_settings_installation()?"))
        self.assertLess(dispatch.index("validate_current_settings_installation()?"),
                        dispatch.index("registration::enable()"))
        self.assertNotIn("run_settings_app", dispatch)
        self.assertNotIn("ai_inference", dispatch)
        main = cli.split("fn main()", 1)[1].split("#[cfg(test)]", 1)[0]
        self.assertLess(main.index("if silent"), main.index("eprintln!"))
        self.assertLess(main.index("if silent"), main.index("MessageBoxW"))
        store = (installer.REPO / "src/startup.rs").read_text(encoding="utf-8-sig")
        self.assertIn("HKEY_CURRENT_USER", store)
        self.assertNotIn("HKEY_LOCAL_MACHINE", store)
        self.assertIn('w!("Flower")', store)
        self.assertIn('Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Run', store)

    def test_post_copy_and_uninstall_validate_before_executing_installed_tool(self):
        source = (installer.REPO / "installer/flower.iss").read_text(encoding="utf-8-sig")
        post = source.split("procedure CurStepChanged", 1)[1].split("procedure CurPageChanged", 1)[0]
        self.assertLess(post.index("SafeInstallPath"), post.index("if Exec(PayloadDir"))
        self.assertNotIn("PrepareInstallPath(", post)
        self.assertNotIn("ExecAsOriginalUser", source)
        self.assertNotIn("install-user-files", source)
        self.assertLess(post.index("TrustedPayload"), post.index("if Exec(PayloadDir"))
        self.assertNotIn("UserFilesFailed", source)
        uninstall = source.split("function InitializeUninstall", 1)[1]
        self.assertLess(uninstall.index("FileCopy("), uninstall.index("SafeInstallPath"))
        self.assertLess(uninstall.index("SafeInstallPath"), uninstall.index("if not Exec(PayloadDir"))
        self.assertLess(uninstall.index("TrustedPayload"), uninstall.index("if not Exec(PayloadDir"))
        self.assertNotIn("PrepareInstallPath(", uninstall)


class InstallerTests(unittest.TestCase):
    def test_base_rejects_unrecorded_model_and_variant_filename_mismatch(self):
        original = self.archive.read_bytes()
        self.rewrite(lambda c: c.update({
            "flower-0.4.7-windows-x64/ai/model_int8.onnx": b"unrecorded",
        }), rehash_payload=True)
        with self.assertRaises(installer.InstallerError):
            installer.verify_package(self.archive)
        self.archive.write_bytes(original)
        self.archive = self.archive.rename(
            self.archive.with_name("flower-0.4.7-windows-x64-ai.zip"))
        self.refresh_external_hash()
        with self.assertRaisesRegex(installer.InstallerError, "变体"):
            installer.verify_package(self.archive)

    def test_new_version_rejects_base_even_with_rehashed_manifest(self):
        def mutate(data):
            name = "flower-0.4.7-windows-x64/manifest.json"
            manifest = json.loads(data[name])
            manifest.update(variant="base", ai={"available": False})
            data[name] = json.dumps(manifest).encode()
            for key in list(data):
                if "/ai/" in key:
                    del data[key]
        self.rewrite(mutate, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "完整 AI"):
            installer.verify_package(self.archive)

    def test_zip_windows_reparse_attribute_is_rejected(self):
        with zipfile.ZipFile(self.archive, "a") as target:
            info = zipfile.ZipInfo("flower-0.4.7-windows-x64/licenses/reparse.txt")
            info.create_system = 0
            info.external_attr = 0x400
            target.writestr(info, b"reparse")
        self.refresh_external_hash()
        with self.assertRaisesRegex(installer.InstallerError, "非普通文件"):
            installer.verify_package(self.archive)

    def test_ai_installer_variant_and_source_zip_association(self):
        (self.base / "nested-assets").mkdir()
        with ai_fixture(self.base / "nested-assets") as (bundle, source, record):
            (self.base / "ai-project").mkdir()
            fixture = PackageFixture(self.base / "ai-project")
            self.archive = flower_package.build_package(
                repo_root=fixture.repo, output_dir=self.base / "ai-zip",
                host_platform="win32", runner=fixture.runner(), ai_assets=bundle,
            ).zip_path
            verified = installer.verify_package(self.archive)
            self.assertEqual(verified.variant, "ai")

            result = self.build()
            self.assertEqual(result.executable.name, "flower-0.4.7-windows-x64-setup.exe")
            output = json.loads(result.manifest.read_bytes())
            self.assertEqual(output["variant"], "ai")
            self.assertEqual(output["source_zip"], self.archive.name)
            self.assertEqual(output["source_zip_sha256"], verified.digest)
            self.assertEqual(output["build_id"], f"0.4.7-{verified.digest[:12]}")

    def test_ai_installer_rejects_partial_unrecorded_extra_and_forged_assets(self):
        (self.base / "nested-assets").mkdir()
        with ai_fixture(self.base / "nested-assets") as (bundle, source, record):
            (self.base / "ai-project").mkdir()
            fixture = PackageFixture(self.base / "ai-project")
            self.archive = flower_package.build_package(
                repo_root=fixture.repo, output_dir=self.base / "ai-zip",
                host_platform="win32", runner=fixture.runner(), ai_assets=bundle,
            ).zip_path
            original = self.archive.read_bytes()
            root = "flower-0.4.7-windows-x64/"

            def manifest_change(contents, mutate):
                obj = json.loads(contents[root + "manifest.json"])
                mutate(obj)
                contents[root + "manifest.json"] = json.dumps(obj).encode()

            mutations = [
                lambda c: c.pop(root + "ai/LICENSE-model.txt"),
                lambda c: c.update({root + "ai/provider.dll": b"extra"}),
                lambda c: c.update({root + "ai/model_int8.onnx": b"forged"}),
                lambda c: c.update({root + "ai/manifest.json": source.read_bytes() + b"\n"}),
                lambda c: manifest_change(c, lambda m: m.pop("ai")),
                lambda c: manifest_change(c, lambda m: m["ai"].update(source_manifest_sha256="0" * 64)),
                lambda c: manifest_change(c, lambda m: m.update(variant="base")),
            ]
            for mutate in mutations:
                self.archive.write_bytes(original)
                self.rewrite(mutate, rehash_payload=True)
                with self.subTest(mutate=mutate):
                    with self.assertRaises(installer.InstallerError):
                        installer.verify_package(self.archive)
            self.assertEqual(self.calls, 0)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        bundle, _, _ = self.enterContext(ai_fixture(self.base))
        fixture = PackageFixture(self.base)
        self.archive = flower_package.build_package(
            repo_root=fixture.repo, output_dir=self.base / "zip",
            host_platform="win32", runner=fixture.runner(), ai_assets=bundle,
        ).zip_path
        self.compiler = self.base / "compiler with spaces" / "ISCC.exe"
        self.compiler.parent.mkdir()
        self.compiler.write_bytes(b"test runner only")
        (self.compiler.parent / "license.txt").write_bytes(
            b"Inno Setup License\nCopyright Jordan Russell\nTest fixture, not a distributable license.\n"
        )
        self.output = self.base / "installer output"
        self.calls = 0

    def runner(self, args, cwd):
        self.calls += 1
        self.assertEqual(cwd, installer.REPO)
        options = {
            arg[2:].split("=", 1)[0]: arg.split("=", 1)[1]
            for arg in args if arg.startswith("/D") and "=" in arg
        }
        payload = Path(options["PackageDir"])
        self.assertEqual(options["FileVersion"], "0.4.7.0")
        include = Path(options["FilesInclude"]).read_text(encoding="utf-8-sig")
        self.assertEqual(len(include.splitlines()), len(list(payload.rglob("*"))) -
                         len([p for p in payload.rglob("*") if p.is_dir()]))
        self.assertNotIn("recursesubdirs", include)
        self.assertIn(r'DestDir: "{app}\versions\{#BuildId}', include)
        self.assertNotIn("{commonpf64}", include)
        for line in include.splitlines():
            self.assertIn(r'DestDir: "{app}\versions\{#BuildId}', line)
        self.assertEqual(
            options["ExpectedDllHash"],
            hashlib.sha256((payload / "flower.dll").read_bytes()).hexdigest(),
        )
        self.assertEqual(
            options["ExpectedSettingsHash"],
            hashlib.sha256((payload / "flower-settings.exe").read_bytes()).hexdigest(),
        )
        stem = next(arg[2:] for arg in args if arg.startswith("/F"))
        expected_stem = self.archive.stem + "-setup"
        self.assertEqual(stem, expected_stem)
        output = Path(options["OutputDir"]) / f"{stem}.exe"
        output.write_bytes(make_pe(is_dll=False))
        return subprocess.CompletedProcess(args, 0, "", "")

    def build(self, **options):
        return installer.build_installer(
            self.archive, self.output, compiler=self.compiler,
            runner=options.pop("runner", self.runner), host="win32", **options,
        )

    def rewrite(self, mutate, *, rehash_payload=False):
        with zipfile.ZipFile(self.archive) as source:
            contents = {info.filename: source.read(info) for info in source.infolist()}
        mutate(contents)
        if rehash_payload:
            sums = next(name for name in contents if name.endswith("/SHA256SUMS"))
            top = sums.rsplit("/", 1)[0]
            contents[sums] = "".join(
                f"{hashlib.sha256(data).hexdigest()}  {name[len(top)+1:]}\n"
                for name, data in sorted(contents.items()) if name != sums
            ).encode()
        with zipfile.ZipFile(self.archive, "w") as target:
            for name, data in contents.items():
                target.writestr(name, data)
        self.refresh_external_hash()

    def refresh_external_hash(self):
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        Path(f"{self.archive}.sha256").write_text(f"{digest}  {self.archive.name}\n")

    def test_valid_package_and_build_hashes_manifest_and_cleanup(self):
        verified = installer.verify_package(self.archive)
        self.assertEqual(verified.version, "0.4.7")
        self.assertIn("data/pinyin/everyday_phrases.tsv", verified.payload)
        self.assertIn("data/pinyin/everyday_phrases.md", verified.payload)
        self.assertEqual(
            hashlib.sha256(verified.payload["data/pinyin/everyday_phrases.tsv"]).hexdigest(),
            installer.pack.EVERYDAY_SHA256,
        )
        result = self.build()
        self.assertEqual(self.calls, 1)
        digest = hashlib.sha256(result.executable.read_bytes()).hexdigest()
        self.assertEqual(result.checksum.read_text().split()[0], digest)
        record = json.loads(result.manifest.read_text())
        self.assertEqual(record["source_zip_sha256"], verified.digest)
        self.assertFalse(record["installation_runtime_verified"])
        self.assertFalse(record["windows_desktop_verified"])
        self.assertNotIn(str(self.base), result.manifest.read_text())
        self.assertEqual(len(list(self.output.iterdir())), 3)

    def test_external_checksum_mismatch_stops_before_compiler(self):
        Path(f"{self.archive}.sha256").write_text("0" * 64 + f"  {self.archive.name}\n")
        with self.assertRaisesRegex(installer.InstallerError, "外部"):
            self.build()
        self.assertEqual(self.calls, 0)

    def test_payload_tamper_is_rejected(self):
        self.rewrite(lambda data: data.update({next(k for k in data if k.endswith("/README.md")): b"changed"}))
        with self.assertRaisesRegex(installer.InstallerError, "哈希"):
            self.build()

    def test_unsafe_paths_multiple_roots_and_extra_payload_are_rejected(self):
        cases = [
            "../outside", "/absolute/file", "root/C:/bad", "root/a\\b",
            "root/CON", "root/CON .txt", "root/COM¹.txt",
            "root/a.", "root/file:stream", "root/{app}",
            "another-root/README.md",
            "flower-0.4.7-windows-x64/unexpected.exe",
            "flower-0.4.7-windows-x64/licenses/evil.exe",
        ]
        original = self.archive.read_bytes()
        for name in cases:
            with self.subTest(name=name):
                self.archive.write_bytes(original)
                self.rewrite(lambda data: data.update({name: b"unsafe"}))
                with self.assertRaises(installer.InstallerError):
                    installer.verify_package(self.archive)

    def test_vocabulary_presence_and_record_fail_closed(self):
        original = self.archive.read_bytes()
        root = "flower-0.4.7-windows-x64/"
        mutations = [
            lambda data: data.update({root + installer.pack.VOCABULARY_PREFIX + "words.txt": b"x"}),
            lambda data: data.update({root + installer.pack.VOCABULARY_PREFIX + "expanded.tsv": b"x"}),
        ]
        def forged_record(data):
            manifest = json.loads(data[root + "manifest.json"])
            manifest["expanded_vocabulary"] = {}
            data[root + "manifest.json"] = json.dumps(manifest).encode()
        mutations.append(forged_record)
        for mutate in mutations:
            self.archive.write_bytes(original)
            self.rewrite(mutate, rehash_payload=True)
            with self.subTest(mutate=mutate), self.assertRaises(installer.InstallerError):
                installer.verify_package(self.archive)

    def test_complete_vocabulary_verified_and_record_tampering_rejected(self):
        from scripts.test_package import dictionary_root
        payload, record = flower_package._vocabulary_bundle(dictionary_root())
        root = "flower-0.4.7-windows-x64/"
        def attach(data):
            data.update({root + path: value for path, value in payload.items()})
            manifest = json.loads(data[root + "manifest.json"])
            manifest["expanded_vocabulary"] = record
            data[root + "manifest.json"] = json.dumps(manifest).encode()
        self.rewrite(attach, rehash_payload=True)
        verified = installer.verify_package(self.archive)
        self.assertTrue(installer.pack.VOCABULARY_FILES.issubset(verified.payload))
        def tamper(data):
            manifest = json.loads(data[root + "manifest.json"])
            manifest["expanded_vocabulary"]["unique_entries"] = 1
            data[root + "manifest.json"] = json.dumps(manifest).encode()
        self.rewrite(tamper, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "记录"):
            installer.verify_package(self.archive)

    def test_new_release_requires_vocabulary(self):
        old_root = "flower-0.4.7-windows-x64/"
        new_root = "flower-0.4.21-windows-x64/"
        new_path = self.archive.with_name(self.archive.name.replace("0.4.7", "0.4.21"))
        self.archive.rename(new_path)
        self.archive = new_path
        def upgrade(data):
            manifest = json.loads(data[old_root + "manifest.json"])
            manifest["version"] = "0.4.21"
            data[old_root + "manifest.json"] = json.dumps(manifest).encode()
            renamed = {name.replace(old_root, new_root, 1): value for name, value in data.items()}
            data.clear()
            data.update(renamed)
        self.rewrite(upgrade, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "扩充词库"):
            installer.verify_package(self.archive)

    def test_audit_release_integrity_and_manifest_cannot_self_sign(self):
        import io
        pack = installer.pack
        values = {name: b"trusted" for name in pack.VOCABULARY_IDENTITIES}
        identities = {name: (len(value), hashlib.sha256(value).hexdigest()) for name, value in values.items()}
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as target:
            for name, value in values.items():
                target.writestr(name, value)
        conversion = {p: (installer.REPO / p).read_bytes() for p in pack.CONVERSION_FILES}
        old_root = "flower-0.4.7-windows-x64/"
        root = "flower-0.4.22-windows-x64/"
        new_path = self.archive.with_name(self.archive.name.replace("0.4.7", "0.4.22"))
        self.archive.rename(new_path)
        self.archive = new_path
        with mock.patch.object(pack, "VOCABULARY_IDENTITIES", identities):
            audit = {pack.VOCABULARY_ARCHIVE: buffer.getvalue()}
            record = pack._verify_vocabulary_archive(audit)
            conversion_record = pack._verify_conversion_payload(conversion)
            def upgrade(data):
                manifest = json.loads(data[old_root + "manifest.json"])
                manifest.update(version="0.4.22", expanded_vocabulary=record,
                                chinese_conversion=conversion_record)
                data[old_root + "manifest.json"] = json.dumps(manifest).encode()
                renamed = {name.replace(old_root, root, 1): value for name, value in data.items()}
                data.clear()
                data.update(renamed)
                data.update({root + p: v for p, v in {**audit, **conversion}.items()})
            self.rewrite(upgrade, rehash_payload=True)
            verified = installer.verify_package(self.archive)
            self.assertEqual(verified.payload[pack.VOCABULARY_ARCHIVE], buffer.getvalue())
            self.assertFalse(pack.VOCABULARY_FILES & verified.payload.keys())
            original = self.archive.read_bytes()
            def manifest_change(data, key):
                manifest = json.loads(data[root + "manifest.json"])
                manifest[key] = {}
                data[root + "manifest.json"] = json.dumps(manifest).encode()
            mutations = [
                lambda data: data.pop(root + pack.VOCABULARY_ARCHIVE),
                lambda data: data.update({root + pack.VOCABULARY_PREFIX + "expanded.tsv": b"trusted"}),
                lambda data: manifest_change(data, "expanded_vocabulary"),
                lambda data: manifest_change(data, "chinese_conversion"),
                lambda data: data.update({root + pack.CONVERSION_PREFIX + "STCharacters.txt": b"changed"}),
                lambda data: data.pop(root + pack.CONVERSION_PREFIX + "NOTICE.md"),
                lambda data: data.update({root + pack.CONVERSION_PREFIX + "extra.txt": b"x"}),
                lambda data: data.update({root + pack.VOCABULARY_ARCHIVE: buffer.getvalue()[:-30]}),
            ]
            for mutate in mutations:
                self.archive.write_bytes(original)
                self.rewrite(mutate, rehash_payload=True)
                with self.subTest(mutate=mutate), self.assertRaises(installer.InstallerError):
                    installer.verify_package(self.archive)
            self.archive.write_bytes(original)
            def raw_only(data):
                data.pop(root + pack.VOCABULARY_ARCHIVE)
                data.update({root + pack.VOCABULARY_PREFIX + name: value for name, value in values.items()})
                manifest = json.loads(data[root + "manifest.json"])
                manifest["expanded_vocabulary"] = pack._verify_vocabulary_payload({
                    pack.VOCABULARY_PREFIX + name: value for name, value in values.items()
                })
                data[root + "manifest.json"] = json.dumps(manifest).encode()
            self.rewrite(raw_only, rehash_payload=True)
            with self.assertRaises(installer.InstallerError):
                installer.verify_package(self.archive)
            legacy_root = "flower-0.4.21-windows-x64/"
            legacy_path = self.archive.with_name(self.archive.name.replace("0.4.22", "0.4.21"))
            self.archive.rename(legacy_path)
            self.archive = legacy_path
            def legacy(data):
                manifest = json.loads(data[root + "manifest.json"])
                manifest["version"] = "0.4.21"
                data[root + "manifest.json"] = json.dumps(manifest).encode()
                renamed = {name.replace(root, legacy_root, 1): value for name, value in data.items()}
                data.clear()
                data.update(renamed)
            self.rewrite(legacy, rehash_payload=True)
            self.assertTrue(pack.VOCABULARY_FILES <= installer.verify_package(self.archive).payload.keys())

    def test_case_insensitive_collision_is_rejected(self):
        self.rewrite(lambda data: data.update({
            "flower-0.4.7-windows-x64/readme.MD": b"duplicate",
        }))
        with self.assertRaises(installer.InstallerError):
            self.build()

    def test_category_allowlist_is_required_and_not_recursive(self):
        self.assertTrue(installer.pack.CATEGORY_FILES <= installer.ROOT_FILES)
        for path in ("data/pinyin/categories/arbitrary.tsv",
                     "data/pinyin/categories/provenance.tsv",
                     "data/pinyin/categories/rejected.tsv",
                     "data/pinyin/sources/thuocl/THUOCL_IT.txt"):
            self.assertNotIn(path, installer.ROOT_FILES)
            with self.subTest(path=path):
                self.rewrite(lambda data: data.update({self.archive.stem + "/" + path: b"extra"}),
                             rehash_payload=True)
                with self.assertRaisesRegex(installer.InstallerError, "白名单"):
                    installer.verify_package(self.archive)

    def test_category_manifest_forgery_rejected_after_outer_rehash(self):
        def mutate(data):
            path = self.archive.stem + "/manifest.json"
            manifest = json.loads(data[path])
            manifest["category_lexicons"]["global_unique_pairs"] += 1
            data[path] = json.dumps(manifest).encode()
        self.rewrite(mutate, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "分类词库记录"):
            installer.verify_package(self.archive)

    def test_self_signed_category_attacks_rejected_after_outer_rehash(self):
        original_archive = self.archive.read_bytes()
        for attack in ("empty", "unsupported_syllables"):
            with self.subTest(attack=attack):
                self.archive.write_bytes(original_archive)
                def mutate(data):
                    prefix = self.archive.stem + "/data/pinyin/categories/"
                    manifest = json.loads(data[prefix + "manifest.json"])
                    if attack == "empty":
                        for name in installer.pack.CATEGORY_TABLES:
                            data[prefix + name] = b""
                            manifest["files"][name].update(
                                bytes=0, lines=0, pairs=0, sha256=hashlib.sha256(b"").hexdigest())
                        coverage = json.loads(data[prefix + "coverage.json"])
                        coverage.update(global_unique_pairs=0, additional_to_default=0)
                        value = json.dumps(coverage).encode()
                        data[prefix + "coverage.json"] = value
                        manifest["files"]["coverage.json"].update(
                            bytes=len(value), lines=len(value.splitlines()),
                            sha256=hashlib.sha256(value).hexdigest())
                        manifest.update(global_unique_pairs=0, additional_to_default=0)
                    else:
                        value = (b"r'zzzz'zzzz\t" + "爱加密".encode() + b"\t1025\n"
                                 + data[prefix + "IT.tsv"].split(b"\n", 1)[1])
                        data[prefix + "IT.tsv"] = value
                        manifest["files"]["IT.tsv"].update(
                            bytes=len(value), sha256=hashlib.sha256(value).hexdigest())
                    data[prefix + "manifest.json"] = json.dumps(manifest).encode()
                self.rewrite(mutate, rehash_payload=True)
                with self.assertRaisesRegex(installer.InstallerError, "可信快照"):
                    installer.verify_package(self.archive)

    def test_zip_symlink_is_rejected(self):
        with zipfile.ZipFile(self.archive, "a") as target:
            info = zipfile.ZipInfo("flower-0.4.7-windows-x64/licenses/link")
            info.create_system = 3
            info.external_attr = 0o120777 << 16
            target.writestr(info, "outside")
        self.refresh_external_hash()
        with self.assertRaisesRegex(installer.InstallerError, "非普通文件"):
            self.build()

    def test_missing_payload_is_rejected(self):
        self.rewrite(lambda data: data.pop(next(k for k in data if k.endswith("/flower.dll"))))
        with self.assertRaises(installer.InstallerError):
            self.build()

    def test_manifest_metadata_mismatch_is_rejected_even_with_rehashed_payload(self):
        def mutate(data):
            name = f"{self.archive.stem}/manifest.json"
            value = json.loads(data[name])
            value["binaries"][0]["size"] = 1
            data[name] = json.dumps(value).encode()
        self.rewrite(mutate, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "元数据"):
            self.build()

    def test_manifest_must_be_a_json_object(self):
        def mutate(data):
            name = f"{self.archive.stem}/manifest.json"
            data[name] = b"[]"
        self.rewrite(mutate, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "JSON"):
            self.build()

    def test_version_must_fit_windows_resource_fields(self):
        def mutate(data):
            name = f"{self.archive.stem}/manifest.json"
            value = json.loads(data[name])
            value["version"] = "70000.4.7"
            data[name] = json.dumps(value).encode()
        self.rewrite(mutate, rehash_payload=True)
        with self.assertRaisesRegex(installer.InstallerError, "版本资源"):
            self.build()

    def test_existing_output_is_preserved(self):
        self.output.mkdir()
        name = self.output / "flower-0.4.7-windows-x64-setup.exe"
        name.write_bytes(b"user-owned")
        with self.assertRaisesRegex(installer.InstallerError, "拒绝覆盖"):
            self.build()
        self.assertEqual(name.read_bytes(), b"user-owned")
        self.assertEqual(self.calls, 0)

    def test_compiler_failure_has_no_products_and_cleans_staging(self):
        def fail(args, cwd):
            return subprocess.CompletedProcess(args, 1, "", "intentional failure")
        with self.assertRaisesRegex(installer.InstallerError, "intentional failure"):
            self.build(runner=fail)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_invalid_compiler_output_is_rejected(self):
        def invalid(args, cwd):
            self.runner(args, cwd)
            output = next(Path(arg.split("=", 1)[1]) for arg in args if arg.startswith("/DOutputDir="))
            next(output.glob("*.exe")).write_bytes(b"not a PE")
            return subprocess.CompletedProcess(args, 0, "", "")
        with self.assertRaisesRegex(installer.InstallerError, "PE"):
            self.build(runner=invalid)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_publish_failure_rolls_back_only_owned_products(self):
        publish = installer.pack._publish_no_overwrite
        calls = 0
        def fail_second(source, destination):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise installer.InstallerError("injected publish failure")
            return publish(source, destination)
        with mock.patch.object(installer.pack, "_publish_no_overwrite", side_effect=fail_second):
            with self.assertRaisesRegex(installer.InstallerError, "injected"):
                self.build()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_non_windows_internal_entry_is_refused(self):
        with self.assertRaisesRegex(installer.InstallerError, "Windows"):
            installer.build_installer(self.archive, self.output, host="linux")

    def test_missing_explicit_compiler_is_not_silently_replaced(self):
        with self.assertRaisesRegex(installer.InstallerError, "编译器"):
            installer.find_compiler(self.base / "missing.exe")

    def test_unrecognized_engine_license_is_rejected(self):
        (self.compiler.parent / "license.txt").write_bytes(b"not a compiler license")
        with self.assertRaisesRegex(installer.InstallerError, "许可证"):
            self.build()
        self.assertEqual(self.calls, 0)

    def test_help_succeeds_without_building(self):
        with mock.patch("sys.stdout"):
            with self.assertRaises(SystemExit) as error:
                installer.main(["--help"])
        self.assertEqual(error.exception.code, 0)


if __name__ == "__main__":
    unittest.main()
