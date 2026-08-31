Stack: Tauri (Rust backend, TS/HTML frontend), whisper.cpp for local STT, Claude Haiku API for summarization.
Platform: Windows only, dev shell is WSL bash. Tauri app runs as a native Windows process, not under WSLg.
Build: `cargo tauri dev` to run (from project root), `cd src-tauri && cargo build` to check Rust compiles (the crate is in src-tauri/, not the root).
Architecture: local snapshot.json is the single source of truth between components — see architecture.md for its schema and current component status. Read architecture.md at the start of every session before writing code.
Paths: anything written from WSL that the Windows-side Tauri app needs to read must go through /mnt/c/... (Windows-visible), never a WSL-only path.
Rules:
- Never commit API keys.
- Never touch /snapshots/ test fixtures directly.
- Don't change the snapshot.json schema without updating architecture.md in the same session.
- End every component with /done-check before considering it finished.

Keep this file under ~15 lines. Add to it only when Claude gets something wrong repeatedly — don't pre-write a manual.
