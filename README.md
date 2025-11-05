# pipr_mvp

Minimal weekend MVP inspired by `pipr`:

- Single-line pipeline input (e.g., `ls -la | grep src | head -n 5`)
- Runs via your shell (`sh -c` / `cmd /C`) and shows stdout/stderr
- History navigation with ↑/↓
- Quit with `q` or `Esc`

## Build & Run

```bash
cd pipr_mvp
cargo run
```

## Next steps

- Multi-stage editor UI (add/remove stages)
- Live streaming output (spawn child and read pipes incrementally)
- Syntax highlighting
- Persistent history
