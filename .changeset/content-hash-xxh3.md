---
default: patch
---

# The clip content hash is XXH3 instead of BLAKE3 (no cryptographic code in the app); the history is moved over on the first start, so repeats of older clips are still found
