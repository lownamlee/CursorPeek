from __future__ import annotations

import codecs
import re
from pathlib import Path


REPOSITORY = Path(__file__).resolve().parents[2]
MANUAL_ROOT = REPOSITORY / "manual-tests"
SNIFF_SOURCE = REPOSITORY / "crates" / "cursorpeek-core" / "src" / "sniff.rs"


def rust_string_array(source: str, name: str) -> list[str]:
    match = re.search(
        rf"pub const {re.escape(name)}:.*?=\s*&\[(.*?)\];",
        source,
        flags=re.DOTALL,
    )
    if match is None:
        raise RuntimeError(f"Could not find Rust array {name}")
    return re.findall(r'"([^"]+)"', match.group(1))


def generic_text(identifier: str) -> str:
    return (
        "CursorPeek manual text fixture\n"
        f"File policy entry: {identifier}\n"
        "Unicode: English | 中文 | العربية | हिन्दी | 🧪\n"
        "This local file is inert test content and contains no real credentials.\n"
    )


def extension_content(extension: str) -> tuple[bytes, str]:
    identifier = f".{extension}"
    if extension == "svg":
        text = (
            '<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" viewBox="0 0 320 180">\n'
            '  <rect width="320" height="180" rx="20" fill="#0f172a"/>\n'
            '  <circle cx="72" cy="90" r="38" fill="#34d399"/>\n'
            '  <text x="128" y="98" fill="#f8fafc" font-size="24">SVG source fixture</text>\n'
            "</svg>\n"
        )
        return text.encode("utf-8"), "utf-8"
    if extension in {"json", "jsonc", "json5"}:
        return (
            (
                '{\n  "fixture": "CursorPeek",\n  "enabled": true,\n'
                '  "unicode": "中文 العربية 🧪"\n}\n'
            ).encode("utf-8"),
            "utf-8",
        )
    if extension in {"jsonl", "ndjson"}:
        return (
            b'{"line":1,"fixture":"CursorPeek"}\n{"line":2,"status":"manual"}\n',
            "utf-8",
        )
    if extension == "csv":
        return b"name,width,height\nCursorPeek,480,300\nLarge,2560,1440\n", "utf-8"
    if extension == "tsv":
        return b"name\twidth\theight\nCursorPeek\t480\t300\nLarge\t2560\t1440\n", "utf-8"
    if extension in {"xml", "plist", "resx", "manifest", "csproj", "vbproj", "vcxproj", "props", "targets", "nuspec"}:
        return (
            (
                '<?xml version="1.0" encoding="utf-8"?>\n'
                f'<cursorpeek-fixture extension="{extension}">manual</cursorpeek-fixture>\n'
            ).encode("utf-8"),
            "utf-8",
        )
    if extension == "reg":
        text = (
            "Windows Registry Editor Version 5.00\r\n\r\n"
            "[HKEY_CURRENT_USER\\Software\\CursorPeekManualFixture]\r\n"
            '"SafeValue"="Not installed by this file"\r\n'
        )
        return codecs.BOM_UTF16_LE + text.encode("utf-16-le"), "utf-16-le-bom"
    if extension in {"pem", "crt", "cer", "csr", "key", "asc"}:
        text = (
            "-----BEGIN CURSORPEEK MANUAL FIXTURE-----\n"
            "Tk9ULUEtUkVBTC1LRVktT1ItTk8tU0VDUkVUUw==\n"
            "-----END CURSORPEEK MANUAL FIXTURE-----\n"
        )
        return text.encode("ascii"), "ascii"
    if extension == "pub":
        return b"ssh-ed25519 Tk9ULUEtUkVBTC1QVUJMSUMtS0VZ cursorpeek-fixture\n", "ascii"
    if extension == "ppk":
        return (
            b"PuTTY-User-Key-File-3: ssh-ed25519\nComment: CursorPeek fake fixture\n"
            b"Public-Lines: 1\nTk9ULUEtUkVBTC1LRVk=\nPrivate-Lines: 0\n",
            "ascii",
        )
    return generic_text(identifier).encode("utf-8"), "utf-8"


def exact_name_content(name: str) -> bytes:
    if name == ".env":
        return b"CURSORPEEK_FIXTURE=true\nAPI_KEY=not-a-real-secret\n"
    if name in {
        ".dockerignore",
        ".eslintignore",
        ".gitignore",
        ".prettierignore",
        "CODEOWNERS",
    }:
        return f"# CursorPeek manual fixture for {name}\n# No active rules.\n".encode("utf-8")
    if name == ".gitattributes":
        return b"# CursorPeek manual fixture. No attributes are configured.\n"
    if name == ".gitmodules":
        return b"# CursorPeek manual fixture. No submodules are configured.\n"
    if name == ".editorconfig":
        return b"root = false\n\n[*]\ncharset = utf-8\n"
    if name in {".eslintrc", ".prettierrc"}:
        return b"{}\n"
    if name == ".npmrc":
        return b"# CursorPeek manual fixture. No npm settings are configured.\n"
    if name == ".nvmrc":
        return b"24\n"
    if name.startswith("id_"):
        return (
            b"-----BEGIN OPENSSH PRIVATE KEY-----\n"
            b"CURSORPEEK-MANUAL-FIXTURE-NOT-A-REAL-KEY\n"
            b"-----END OPENSSH PRIVATE KEY-----\n"
        )
    if name in {"known_hosts", "authorized_keys"}:
        return b"example.invalid ssh-ed25519 Tk9ULUEtUkVBTC1LRVk=\n"
    if name == "VERSION":
        return b"0.0.0-fixture\n"
    return generic_text(name).encode("utf-8")


def write_encoding_scenarios(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    text = "CursorPeek encoding fixture — 中文 — café — Ελληνικά\nSecond line.\n"
    (directory / "utf-8.txt").write_bytes(text.encode("utf-8"))
    (directory / "utf-8-bom.txt").write_bytes(codecs.BOM_UTF8 + text.encode("utf-8"))
    (directory / "utf-16-le.txt").write_bytes(codecs.BOM_UTF16_LE + text.encode("utf-16-le"))
    (directory / "utf-16-be.txt").write_bytes(codecs.BOM_UTF16_BE + text.encode("utf-16-be"))
    (directory / "utf-32-le.txt").write_bytes(codecs.BOM_UTF32_LE + text.encode("utf-32-le"))
    (directory / "utf-32-be.txt").write_bytes(codecs.BOM_UTF32_BE + text.encode("utf-32-be"))
    (directory / "windows-1252.txt").write_bytes("CursorPeek — café — naïve\r\n".encode("cp1252"))
    (directory / "shift-jis.txt").write_bytes("CursorPeek 手動テスト\r\n".encode("shift_jis"))
    (directory / "long-text.txt").write_text(
        "".join(f"{index:04d}: CursorPeek bounded text preview line with Unicode 🧪\n" for index in range(400)),
        encoding="utf-8",
        newline="\n",
    )
    (directory / "binary-disguised-as-text.txt").write_bytes(
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01"
    )


def write_text_scenarios(directory: Path) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "empty.txt").write_bytes(b"")
    (directory / "one-line.txt").write_text(
        "One short CursorPeek line.\n",
        encoding="utf-8",
        newline="\n",
    )
    (directory / "long-line.txt").write_text(
        "CursorPeek " + ("0123456789" * 600) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    (directory / "many-lines.txt").write_text(
        "".join(f"Manual preview line {index:03d}\n" for index in range(250)),
        encoding="utf-8",
        newline="\n",
    )
    (directory / "mixed-newlines.txt").write_bytes(b"CRLF\r\nLF\nCR\rFinal line\n")
    (directory / "multilingual.txt").write_text(
        "English\n中文\nالعربية\nहिन्दी\nעברית\nไทย\nEmoji: 🧪 📄 🎞️\n",
        encoding="utf-8",
        newline="\n",
    )


def main() -> None:
    source = SNIFF_SOURCE.read_text(encoding="utf-8")
    extensions = rust_string_array(source, "TEXT_EXTENSIONS")
    names = rust_string_array(source, "TEXT_NAMES")

    extensions_directory = MANUAL_ROOT / "text" / "extensions"
    names_directory = MANUAL_ROOT / "text" / "exact-names"
    extensions_directory.mkdir(parents=True, exist_ok=True)
    names_directory.mkdir(parents=True, exist_ok=True)

    for extension in extensions:
        payload, _encoding = extension_content(extension)
        (extensions_directory / f"sample.{extension}").write_bytes(payload)
    for name in names:
        (names_directory / name).write_bytes(exact_name_content(name))

    write_encoding_scenarios(MANUAL_ROOT / "text" / "encodings")
    write_text_scenarios(MANUAL_ROOT / "text" / "scenarios")

    print(f"text_extensions={len(extensions)}")
    print(f"exact_names={len(names)}")


if __name__ == "__main__":
    main()
