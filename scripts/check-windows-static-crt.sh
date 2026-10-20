#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-windows-static-crt.sh — fail when a Windows binary imports the
# Microsoft C runtime.
#
#     dumpbin -nologo -dependents apprafter.exe | scripts/check-windows-static-crt.sh
#     scripts/check-windows-static-crt.sh --self-test
#
# WHY. Rust's MSVC target links the C runtime dynamically unless the build
# passes `-C target-feature=+crt-static`. Such a binary imports
# VCRUNTIME140.dll and the UCRT's api-ms-win-crt-*.dll forwarders. Every
# GitHub Windows runner has those (it carries Visual Studio), so the
# release workflow's own `--version` check passes, and the first to notice
# is a user on a clean PC who gets "VCRUNTIME140.dll was not found" before
# anything runs. release-cli.yml links statically and runs this over the
# binary it is about to publish.
#
# WHAT COUNTS as the C runtime: vcruntime*.dll, msvcp*.dll (the C++
# library), ucrtbase*.dll and api-ms-win-crt-*.dll. The other api-ms-win-*
# sets (api-ms-win-core-synch-l1-2-0.dll, which Rust's std imports for
# WaitOnAddress) belong to Windows itself and are allowed.
#
# Input is dumpbin's text: one DLL name per indented line, CRLF on
# Windows. Input naming no DLL at all fails (exit 2) instead of passing:
# a guard that read nothing is not a guard. --self-test runs the check over
# built-in samples, so CI proves the pattern still matches dumpbin's
# format each time it relies on it.
set -euo pipefail

CRT='^(vcruntime[0-9a-z_]*|msvcp[0-9a-z_]*|ucrtbase[a-z_]*|api-ms-win-crt-[0-9a-z-]+)\.dll$'

# The DLL names in dumpbin output, lower-cased, one per line. dumpbin
# indents each name; nothing else it prints is a bare `<name>.dll` line.
dll_names() {
    tr -d '\r' \
        | sed -n 's/^[[:space:]]\{1,\}\([A-Za-z0-9_.-]\{1,\}\.[Dd][Ll][Ll]\)[[:space:]]*$/\1/p' \
        | tr '[:upper:]' '[:lower:]'
}

# Reads dumpbin output on stdin. 0: no C-runtime import; 1: one or more; 2: no DLL names.
check() {
    local names crt dll
    names="$(dll_names)"
    if [ -z "$names" ]; then
        echo "::error::no DLL names in the input; expected the output of dumpbin -dependents" >&2
        return 2
    fi
    crt="$(grep -E "$CRT" <<<"$names" || true)"
    if [ -n "$crt" ]; then
        echo "::error::the binary imports the C runtime, so it will not start on a PC without the Visual C++ Redistributable:" >&2
        while IFS= read -r dll; do
            echo "  $dll" >&2
        done <<<"$crt"
        echo "Link it with -C target-feature=+crt-static (CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS)." >&2
        return 1
    fi
    echo "OK: $(wc -l <<<"$names" | tr -d ' ') imported DLLs, none of them the C runtime"
}

HEADER=$'Microsoft (R) COFF/PE Dumper Version 14.44.35213.0\r\nCopyright (C) Microsoft Corporation.  All rights reserved.\r\n\r\n\r\nDump of file cli/target/x86_64-pc-windows-msvc/release/apprafter.exe\r\n\r\nFile Type: EXECUTABLE IMAGE\r\n\r\n  Image has the following dependencies:\r\n\r\n'
STATIC=$'    KERNEL32.dll\r\n    ADVAPI32.dll\r\n    ntdll.dll\r\n    bcryptprimitives.dll\r\n    api-ms-win-core-synch-l1-2-0.dll\r\n    WS2_32.dll\r\n    USERENV.dll\r\n'
VCRUNTIME=$'    VCRUNTIME140.dll\r\n'
UCRT=$'    api-ms-win-crt-runtime-l1-1-0.dll\r\n    api-ms-win-crt-heap-l1-1-0.dll\r\n'
LOWER=$'    vcruntime140_1.dll\r\n'
FOOTER=$'\r\n  Summary\r\n\r\n        1000 .data\r\n        1000 .rdata\r\n'

# expect <rc> <what> <input>: run check over <input>, require exit <rc>.
expect() {
    local want="$1" what="$2" input="$3" rc=0
    check <<<"$input" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne "$want" ]; then
        echo "self-test FAILED: $what (exit $rc, want $want)" >&2
        return 1
    fi
}

self_test() {
    expect 0 "a statically linked import list is accepted" "${HEADER}${STATIC}${FOOTER}"
    expect 1 "VCRUNTIME140.dll is refused" "${HEADER}${STATIC}${VCRUNTIME}${FOOTER}"
    expect 1 "the UCRT forwarders alone are refused" "${HEADER}${STATIC}${UCRT}${FOOTER}"
    expect 1 "a lower-case vcruntime140_1.dll is refused" "${HEADER}${STATIC}${LOWER}${FOOTER}"
    expect 2 "input that names no DLL is refused" "${HEADER}${FOOTER}"
    echo "OK: self-test (5 cases)"
}

case "${1:-}" in
    --self-test) self_test ;;
    "") check ;;
    *) echo "usage: $0 [--self-test] < dumpbin-output" >&2; exit 2 ;;
esac
