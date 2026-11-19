#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["blake3"]
# ///
"""Test helper: replace an invite token's ephemeral secret with random bytes and fix the
checksum, so the token is well-formed (CONTRACTS.md section 7) but its holder cannot sign for
the invite's key. Prints the forged token. Used by tools/test-room-join.py (role forgedsig)."""
import base64, os, struct, sys, blake3

tok = sys.argv[1].strip()
assert tok.startswith("prim1")
raw = base64.b32decode(tok[5:].upper() + "=" * (-len(tok[5:]) % 8))
body, check = raw[:-4], raw[-4:]
assert blake3.blake3(body, derive_key_context="prim/1 token-check").digest()[:4] == check, "real token expected"
rec_len = struct.unpack("<H", body[33:35])[0]  # bytes16 = u16 length prefix (little-endian)
secret_at = 35 + rec_len
assert secret_at + 32 <= len(body)
forged = body[:secret_at] + os.urandom(32) + body[secret_at + 32:]
c = blake3.blake3(forged, derive_key_context="prim/1 token-check").digest()[:4]
print("prim1" + base64.b32encode(forged + c).decode().rstrip("=").lower())
