# PipeTUI

## Features

- Single-line pipeline input (e.g., `ls -la | grep src | head -n 5`)
- Executes through the host shell (`sh -c` / `cmd /C`) and displays stdout/stderr
- History navigation with ↑/↓ plus persistent storage between runs
- Editing with ←/→, Home/End, Ctrl+A/E/U, Backspace/Delete
- Status line showing exit code and key bindings; quit with `Esc` or `Ctrl+C`
- Live streaming of process output instead of waiting for command completion

## Build & Run

```bash
git clone https://github.com/mel-edo/pipetui
cd pipetui
cargo run
```