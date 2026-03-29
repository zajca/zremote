#!/usr/bin/env python3
"""Patch UniFFI-generated Kotlin bindings for Android compatibility.

Fixes:
1. FfiException subclasses: `val message` conflicts with Throwable.message
   - Makes constructor param `override val message`
   - Removes duplicate getter
2. Adds OVERLOAD_RESOLUTION_AMBIGUITY suppress
"""
import sys

path = sys.argv[1]
with open(path, "r") as f:
    lines = f.readlines()

result = []
i = 0
while i < len(lines):
    line = lines[i]
    # Match: val `message`: kotlin.String inside FfiException subclass
    if "val `message`: kotlin.String" in line:
        context = "".join(lines[max(0, i - 5) : min(len(lines), i + 10)])
        if "FfiException()" in context:
            line = line.replace("val `message`", "override val `message`")
            result.append(line)
            i += 1
            # Skip the duplicate override getter
            while i < len(lines):
                cur = lines[i].strip()
                if cur == "override val message":
                    i += 1  # skip "override val message"
                    if i < len(lines) and "get()" in lines[i]:
                        i += 1  # skip "get() = ..."
                    break
                else:
                    result.append(lines[i])
                    i += 1
            continue
    result.append(line)
    i += 1

# Add suppress annotation
output = "".join(result)
output = output.replace(
    '@file:Suppress("NAME_SHADOWING")',
    '@file:Suppress("NAME_SHADOWING", "OVERLOAD_RESOLUTION_AMBIGUITY")',
)

with open(path, "w") as f:
    f.write(output)

print(f"Patched {path}")
