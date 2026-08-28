# The state seam

`panedrive` reads your UI's state from a **JSON file**, not from the screen. That
file is the *seam*. It is deliberately language-agnostic: the Rust
[`paneview`](../paneview) crate is a convenience, not a requirement. Any program
in any language can expose a seam by writing the same file, and every
`panedrive` feature (`wait-until`, `assert`, `run`) then works against it.

## The contract

1. **One JSON object.** The file contains a single JSON object describing the
   state you want to assert on. The shape is entirely yours; conditions address
   it by dot-path (`bag.count`, `rows.2.status`), and array indices are path
   segments.
2. **Write it atomically.** Write to a temp file, then `rename` it over the real
   path. `rename` is atomic on the same filesystem, so a reader never sees a
   half-written file. `panedrive` treats an unreadable/partial file as "not
   ready yet" and keeps polling, so a non-atomic write only costs you flakiness.
3. **Write on every state change.** Emit a fresh snapshot whenever state changes
   (for a TUI, once per render is the simplest correct choice). If you forget to
   snapshot after a change, `wait-until` will just time out.
4. **Keep values scalar where you assert.** Conditions compare scalar leaves
   (string/number/bool). Numbers and booleans compare by text (`count=2` matches
   `2` or `2.0`); `~=` matches substrings; `>` `<` `>=` `<=` compare numerically.

Drive it the same way regardless of language:

```bash
panedrive wait-until "ready=true" --state app.state.json
panedrive assert     "screen=list" --state app.state.json
```

## Adapters

Each of these writes the same seam. Call the writer wherever state changes.

### Rust (`paneview`)

```rust
use paneview::{write_snapshot, DumpState};

impl DumpState for App {
    fn dump_state(&self) -> serde_json::Value {
        serde_json::json!({ "screen": self.screen_name(), "count": self.count })
    }
}
write_snapshot(&app, std::path::Path::new("app.state.json"))?; // atomic
```

### Go

```go
func writeSeam(path string, state any) error {
    b, err := json.Marshal(state)
    if err != nil {
        return err
    }
    tmp := path + ".tmp"
    if err := os.WriteFile(tmp, b, 0o644); err != nil {
        return err
    }
    return os.Rename(tmp, path) // atomic
}
// writeSeam("app.state.json", map[string]any{"screen": "list", "count": 2})
```

### Python

```python
import json, os

def write_seam(path, state):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, path)  # atomic

# write_seam("app.state.json", {"screen": "list", "count": 2})
```

### Node / JavaScript

```js
const fs = require("fs");

function writeSeam(path, state) {
  const tmp = path + ".tmp";
  fs.writeFileSync(tmp, JSON.stringify(state));
  fs.renameSync(tmp, path); // atomic
}
// writeSeam("app.state.json", { screen: "list", count: 2 });
```

## No seam? Fall back to the screen

If you cannot instrument the target (a third-party TUI), `panedrive run
--from-capture` evaluates conditions against the captured screen text instead of
a seam file: the whole screen is at `screen` and each row at `lines.<n>`, so
`screen~=Ready` or `lines.0~=Loading` work. It is less precise than a real seam
(layout-sensitive), but it degrades gracefully instead of not working at all.
