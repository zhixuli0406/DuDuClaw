"""Portable-binary guard: synthetic PE parsing and mocked otool (stdlib only)."""
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check_portable_binary.py"
spec = importlib.util.spec_from_file_location("portable", SCRIPT)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def build_pe(imports=(), delay=(), pe32=False) -> bytes:
    """One .idata section at RVA 0x1000 / file offset 0x200."""
    pe_off = 0x40
    opt_size = 96 + 16 * 8 if pe32 else 112 + 16 * 8
    dd_off = pe_off + 24 + (96 if pe32 else 112)
    sect_off = pe_off + 24 + opt_size
    rva_base, file_base = 0x1000, 0x200
    body = bytearray()

    def add_str(s):
        rva = rva_base + len(body)
        body.extend(s.encode() + b"\0")
        return rva

    imp_names = [add_str(n) for n in imports]
    delay_names = [add_str(n) for n in delay]
    while len(body) % 4:
        body.append(0)
    imp_rva = rva_base + len(body)
    for rva in imp_names:
        body.extend(struct.pack("<IIIII", 0, 0, 0, rva, 0))
    body.extend(b"\0" * 20)
    delay_rva = rva_base + len(body)
    for rva in delay_names:
        body.extend(struct.pack("<IIIIIIII", 1, rva, 0, 0, 0, 0, 0, 0))
    body.extend(b"\0" * 32)

    data = bytearray(file_base)
    data[0:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, pe_off)
    data[pe_off:pe_off + 4] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, pe_off + 4, 0x8664, 1, 0, 0, 0, opt_size, 0)
    struct.pack_into("<H", data, pe_off + 24, 0x10B if pe32 else 0x20B)
    struct.pack_into("<I", data, dd_off - 4, 16)
    if imports:
        struct.pack_into("<II", data, dd_off + 8, imp_rva, 0)
    if delay:
        struct.pack_into("<II", data, dd_off + 13 * 8, delay_rva, 0)
    struct.pack_into("<8sIIII", data, sect_off, b".idata", len(body), rva_base,
                     len(body), file_base)
    return bytes(data) + bytes(body)


class PeTests(unittest.TestCase):
    def test_parses_import_and_delay_tables(self):
        for pe32 in (False, True):
            data = build_pe(["KERNEL32.dll", "bcrypt.dll"], ["libssl-3-x64.dll"], pe32)
            self.assertEqual(guard.pe_imported_dlls(data),
                             ["KERNEL32.dll", "bcrypt.dll", "libssl-3-x64.dll"])

    def test_clean_pe_passes_and_openssl_fails(self):
        clean = build_pe(["KERNEL32.dll", "ws2_32.dll"])
        self.assertEqual(guard.check_pe(Path("a.exe"), clean), [])
        for dll in ("libssl-3-x64.dll", "LIBCRYPTO-3.dll", "ssleay32.dll", "libeay32.dll"):
            with self.subTest(dll=dll):
                errors = guard.check_pe(Path("a.exe"), build_pe(["KERNEL32.dll", dll]))
                self.assertEqual(len(errors), 1)
                self.assertIn(dll, errors[0])
        errors = guard.check_pe(Path("a.exe"), build_pe(["KERNEL32.dll"], ["libcrypto-3-x64.dll"]))
        self.assertEqual(len(errors), 1)

    def test_garbage_pe_reports_error(self):
        self.assertTrue(guard.check_pe(Path("a.exe"), b"MZ" + b"\0" * 100))

    def test_cli_exit_codes(self):
        with tempfile.TemporaryDirectory() as directory:
            good = Path(directory) / "good.exe"
            bad = Path(directory) / "bad.exe"
            good.write_bytes(build_pe(["KERNEL32.dll"]))
            bad.write_bytes(build_pe(["libssl-3-x64.dll"]))
            run = lambda p: subprocess.run([sys.executable, str(SCRIPT), str(p)],
                                           capture_output=True, text=True)
            self.assertEqual(run(good).returncode, 0)
            result = run(bad)
            self.assertEqual(result.returncode, 1)
            self.assertIn("ERROR:", result.stderr)


OTOOL_OK = """/x/duduclaw:
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)
\t/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1.0.0, current version 61.0.0)
\t@rpath/libfoo.dylib (compatibility version 1.0.0, current version 1.0.0)
"""
OTOOL_BAD = OTOOL_OK + "\t/opt/homebrew/opt/openssl@4/lib/libssl.4.dylib (compatibility version 4.0.0, current version 4.0.0)\n"


class MachoTests(unittest.TestCase):
    def test_detect_formats(self):
        self.assertEqual(guard.detect_format(b"\xcf\xfa\xed\xfe" + b"0"), "macho")
        self.assertEqual(guard.detect_format(b"\xca\xfe\xba\xbe" + b"0"), "macho")
        self.assertEqual(guard.detect_format(b"MZ\0\0"), "pe")
        self.assertEqual(guard.detect_format(b"\x7fELF"), "elf")

    def _run(self, output):
        completed = subprocess.CompletedProcess([], 0, stdout=output, stderr="")
        with patch.object(guard.subprocess, "run", return_value=completed):
            return guard.check_macho(Path("/x/duduclaw"))

    def test_system_only_passes(self):
        self.assertEqual(self._run(OTOOL_OK), [])

    def test_homebrew_dependency_fails(self):
        errors = self._run(OTOOL_BAD)
        self.assertEqual(len(errors), 1)
        self.assertIn("/opt/homebrew/opt/openssl@4/lib/libssl.4.dylib", errors[0])

    def test_otool_failure_is_an_error(self):
        with patch.object(guard.subprocess, "run",
                          side_effect=subprocess.CalledProcessError(1, "otool")):
            self.assertEqual(len(guard.check_macho(Path("/x"))), 1)

    def test_elf_is_noted_and_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bin"
            path.write_bytes(b"\x7fELF" + b"\0" * 16)
            self.assertEqual(guard.check_binary(path), [])


if __name__ == "__main__":
    unittest.main()
