---
title: Fixture
status: draft
---

# Provenance

Every node carries a [source span](https://example.invalid/spec#span).

<!-- kept as a comment node -->

## Byte ranges

Text formats record a byte range and a line range.

```rust
fn span() -> u32 {
    1
}
```

- exact for native parsers
- degraded for converters

| tier | provenance |
|---|---|
| T1 | exact |
| T5 | metadata |
