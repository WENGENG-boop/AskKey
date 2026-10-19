#!/usr/bin/env python3
"""Enforce fixed repository hygiene and production boundaries."""

import argparse
from collections import Counter
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tokenize

PATTERN_EXCEPTIONS = {
    "scripts/check_hygiene.py",
    "Tests/Automation/test_check_hygiene.py",
}
MULTICA_EXCEPTIONS = {"docs/features.md"}
CJK_ALLOWLIST = {
    "Sources/AskKeyAppKit/Resources/Localizable.xcstrings",  # English and Simplified Chinese UI catalog.
    "Sources/AskKeyHelper/Resources/Localizable.xcstrings",  # English and Simplified Chinese helper copy (#83).
    "README.zh-CN.md",  # Simplified Chinese README.
    "site/index.html",  # Language switcher on the English website (#160).
    "site/zh/index.html",  # Simplified Chinese website (#160).
    "site/404.html",  # Bilingual not-found page of the website (#160).
    "windows/README.zh-CN.md",  # Simplified Chinese README of the Windows edition.
    "windows/app/ui/i18n.js",  # English and Simplified Chinese UI strings of the Windows edition.
    "docs/glossary.md",  # Chinese UI terms alongside English glossary prose (#67).
    "Sources/AskKeyAppKit/Resources/zh-Hans.lproj/InfoPlist.strings",  # Localized product name.
    "Tests/AskKeyAppTests/AgentClientConnectionPolicyTests.swift",  # Chinese client connection errors.
    "Tests/AskKeyAppTests/AppLanguageCatalogTests.swift",  # Chinese catalog and bundle-name assertions.
    "Tests/AskKeyAppTests/AppLanguageExperienceTests.swift",  # Chinese language switching and persistence.
    "Tests/AskKeyAppTests/Batch4SettingsLanguageTests.swift",  # Chinese settings and notification copy.
    "Tests/AskKeyAppTests/CredentialEditorVisualTests.swift",  # Chinese credential editor and presentation copy.
    "Tests/AskKeyAppTests/CredentialWorkspaceExperienceTests.swift",  # Chinese workspace text assertions.
    "Tests/AskKeyAppTests/ExpiryReminderAsyncBoundaryTests.swift",  # Chinese expiry notification assertions.
    "Tests/AskKeyAppTests/LocalizationRemediationTests.swift",  # Chinese localization regression assertions.
    "Tests/AskKeyAppTests/LocalizationUnificationTests.swift",  # Chinese shared localization behavior.
    "Tests/AskKeyAppTests/ManagementAuthenticationProcessTests.swift",  # Chinese authentication prompts.
    "Tests/AskKeyAppTests/ManagementAuthenticationTests.swift",  # Chinese vault and authentication errors.
    "Tests/AskKeyAppTests/OnboardingFlowTests.swift",  # Chinese app-opening copy.
    "Tests/AskKeyAppTests/ReviewEditorMissingCredentialTests.swift",  # Chinese visible editor controls.
    "Tests/AskKeyAppTests/ReviewEnvImportTests.swift",  # Chinese environment-import error copy.
    "Tests/AskKeyAppTests/ReviewLocalizationTests.swift",  # Chinese catalog translations.
    "Tests/AskKeyAppTests/ReviewPrivateNotesTests.swift",  # Chinese private-note placeholder assertions.
    "Tests/AskKeyAppTests/WorkspaceInteractionTests.swift",  # Chinese workspace and approval labels.
    "Tests/AskKeyAppTests/WorkspacePrototypeContractTests.swift",  # Chinese UI contracts and visual fixtures.
    "Tests/AskKeyAppTests/WorkspaceVisualContractTests.swift",  # Chinese window-title assertions.
    "Tests/AskKeyIntegrationsTests/HelperLocalizationTests.swift",  # Chinese helper localization assertions (#83).
    "Tests/AskKeyE2ETests/AskKeyE2ETests.swift",  # Chinese onboarding controls and completion messages.
    "Tests/AskKeyE2ETests/CredentialE2ETests.swift",  # Chinese visible credential controls.
    "Tests/AskKeyVaultTests/CredentialManagementVocabularyTests.swift",  # Chinese file-label assertions.
    "Tests/AskKeyVaultTests/LocalVaultLifecycleTests.swift",  # Chinese erase-confirmation assertions.
}
BRAND_TOKEN = "请旨"
CJK = re.compile(
    r"[\u1100-\u11ff\u2e80-\u303f\u3040-\u30ff\u3100-\u318f\u31a0-\u31ff"
    r"\u3400-\u4dbf\u4e00-\u9fff\ua960-\ua97f\uac00-\ud7ff\uf900-\ufaff"
    r"\uff01-\uff0f\uff1a-\uff20\uff3b-\uff40\uff5b-\uffdc"
    r"\U0001aff0-\U0001afff\U0001b000-\U0001b16f\U00020000-\U0002ffff\U00030000-\U0003ffff]"
)
I18N_MARKER = re.compile(r"(?://|#)\s*i18n-literal:\s*(\S.*?)\s*$")
TEST_SUPPORT = re.compile(r"E2E|Fixture|Probe|RestartProof|DebugSupport|RealUIInput")
DECLARATION = re.compile(r"\b(?:class|struct|enum|actor|protocol|typealias)\s+([A-Za-z_][A-Za-z_0-9]*)")
DEBUG = re.compile(r"#if\s+DEBUG\b")
DEBUG_ALLOWLIST = {
    "Sources/AskKeyAppKit/AgentClientConnector.swift",  # Isolated client configuration home.
    "Sources/AskKeyAppKit/AppPreferences.swift",  # Isolated preferences suite.
    "Sources/AskKeyAppKit/App/AppRuntimeState.swift",  # Require the packaged development run directory.
    "Sources/AskKeyBroker/BrokerConfiguration.swift",  # Development broker socket namespace.
    "Sources/AskKeyBroker/DebugRunDirectory.swift",  # Validate the development run directory.
    "Sources/AskKeyVault/Keychain/IsolatedAppKeyStore.swift",  # File-backed development keys.
    "Sources/AskKeyVault/Keychain/KeychainStore.swift",  # Reject keychain I/O in isolated runs.
    "Sources/AskKeyVault/Vault.swift",  # Select the isolated development key store.
    "Sources/AskKeyVault/VaultConfiguration.swift",  # Development data and keychain namespace.
    "Sources/AskKeyHelper/OpenHostApplication.swift",  # Resolve the development host app.
}
SWIFT_NON_CODE = re.compile(
    r'//[^\n]*|/\*|(?:\#+)?"""|(?:\#+)?"|\#+/', re.MULTILINE
)


def swift_interpolation_end(text, offset, line_comments):
    """Skip balanced interpolation code, including its nested literals/comments."""
    depth = 1
    while depth and offset < len(text):
        if match := SWIFT_NON_CODE.match(text, offset):
            offset = swift_non_code_end(text, match.group(), match.end(), line_comments)
        else:
            depth += (text[offset] == "(") - (text[offset] == ")")
            offset += 1
    return offset


def swift_non_code_end(text, token, end, line_comments=None):
    if token.startswith("//"):
        if line_comments is not None:
            line_comments.append((end - len(token), token))
    elif token == "/*":
        depth = 1
        while depth and end < len(text):
            nested = re.search(r"/\*|\*/", text[end:])
            if nested is None:
                return len(text)
            depth += 1 if nested.group() == "/*" else -1
            end += nested.end()
    else:
        hashes = len(token) - len(token.lstrip("#"))
        delimiter = token[hashes:]
        closing, escape = delimiter + "#" * hashes, "\\" + "#" * hashes
        while end < len(text):
            if text.startswith(escape + "(", end):
                end = swift_interpolation_end(text, end + len(escape) + 1, line_comments)
            elif text.startswith(escape, end):
                end += len(escape) + 1
            elif delimiter == "/" and text[end] == "\\":
                end += 2
            elif text.startswith(closing, end):
                return end + len(closing)
            else:
                end += 1
    return end


def swift_non_code_spans(text, line_comments=None):
    """Find comments and literals without treating their contents as code."""
    start = 0
    while match := SWIFT_NON_CODE.search(text, start):
        token = match.group()
        end = swift_non_code_end(text, token, match.end(), line_comments)
        yield token, match.start(), end
        start = end


def declaration_names(text):
    """Ignore comments and string literals when looking for declared types."""
    pieces, start = [], 0
    for _, begin, end in swift_non_code_spans(text):
        pieces.extend([text[start:begin], " "])
        start = end
    pieces.append(text[start:])
    return DECLARATION.findall("".join(pieces))


def marked_literal_lines(name, text):
    """Accept only real Swift or Python line comments with an English reason."""
    if name.endswith(".swift"):
        line_comments = []
        for _ in swift_non_code_spans(text, line_comments):
            pass
        comments = [(text[:begin].count("\n") + 1, comment) for begin, comment in line_comments]
    elif name.endswith(".py"):
        try:
            comments = [(token.start[0], token.string) for token in
                        tokenize.generate_tokens(io.StringIO(text).readline)
                        if token.type == tokenize.COMMENT]
        except (tokenize.TokenError, IndentationError):
            return set()
    else:
        return set()
    return {line for line, comment in comments if (match := I18N_MARKER.fullmatch(comment))
            and not CJK.search(match[1])}


def has_unapproved_cjk(name, text):
    if name in CJK_ALLOWLIST:
        return False
    text = text.replace(BRAND_TOKEN, "")
    if not CJK.search(text):
        return False
    marked = marked_literal_lines(name, text)
    return any(CJK.search(line) and number not in marked
               for number, line in enumerate(text.split("\n"), 1))


def tracked_paths(root):
    raw = subprocess.check_output(["git", "ls-files", "-z"], cwd=root)
    return sorted({os.fsdecode(path) for path in raw.split(b"\0") if path})


def tracked_bytes(path):
    # A tracked symlink stores its link text, never the contents of its target.
    if path.is_symlink():
        return os.fsencode(os.readlink(path))
    if not path.is_file():
        return None
    return path.read_bytes()


def text_content(data):
    for marker, encoding in [(b"\xff\xfe\0\0", "utf-32"), (b"\0\0\xfe\xff", "utf-32"),
                             (b"\xff\xfe", "utf-16"), (b"\xfe\xff", "utf-16")]:
        if data.startswith(marker):
            try:
                return data.decode(encoding)
            except UnicodeDecodeError:
                return None
    if b"\0" in data:
        return None
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


def violations(root):
    entries = set()
    for name in tracked_paths(root):
        path = root / name
        if not name.isascii():
            entries.add(f"non-ascii-name:{name}")
        data = tracked_bytes(path)
        text = text_content(data) if data is not None else None
        if (text is not None and path.suffix in {".swift", ".py", ".sh", ".c", ".h"}
                and len(text.splitlines()) > 600):
            entries.add(f"size:{name}")
        if text is not None and has_unapproved_cjk(name, text):
            entries.add(f"cjk:{name}")
        if name in PATTERN_EXCEPTIONS:
            continue
        if name.startswith("Sources/"):
            if TEST_SUPPORT.search(name) or (text is not None and any(
                    TEST_SUPPORT.search(type_name) for type_name in declaration_names(text))):
                entries.add(f"test-support:{name}")
            count = len(DEBUG.findall(text)) if text is not None else 0
            if count and name not in DEBUG_ALLOWLIST:
                entries.add(f"debug:{name}:{count}")
        if name != "AGENTS.md":
            if text is not None and ("/Users/" in text or "/private/var/" in text):
                entries.add(f"local-path:{name}")
            if name not in MULTICA_EXCEPTIONS and ((data is not None and b"multica" in data.lower())
                    or (text is not None and "multica" in text.casefold())):
                entries.add(f"multica:{name}")
    return entries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    root = Path(subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True).strip())
    current = violations(root)
    for entry in sorted(current):
        print(f"violation: {entry}")
    if current:
        return 1
    counts = Counter(entry.split(":", 1)[0] for entry in current)
    print("Hygiene rules passed: " + ", ".join(
        f"{check}={counts.get(check, 0)}" for check in
        ["size", "test-support", "debug", "local-path", "non-ascii-name", "multica", "cjk"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
