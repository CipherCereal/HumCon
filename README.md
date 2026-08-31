# HumCon

A Windows desktop "resume card." HumCon quietly captures what you were doing —
your active window, recent WSL shell commands, a hotkey-triggered voice note
(recorded, transcribed locally by whisper.cpp, and summarized by the Claude
Haiku API), and your active browser tab — into one local `snapshot.json`, and
renders it in a small window so you can see what you were doing when you come
back to your desk.

## How it's put together

Four independent producers write into one shared `snapshot.json`; one card
reads it back:

- **Window/app capture** — polls the foreground window every 3s, writes `active_window`.
- **Shell command log** — a bash hook (sourced from `~/.bashrc`) appends each interactive
  command to `commands.jsonl`; the app tails that file every 2s into `recent_commands`.
- **Voice note** — `Ctrl+Alt+Space` toggles recording; the WAV is transcribed locally by
  `whisper-cli.exe` and the transcript is summarized by the Claude Haiku API, into `voice_note`.
- **Browser extension** — reports the active tab to a local loopback server on
  `127.0.0.1:7423`, writing `browser_tab`.
- **Resume card** — the app's UI, reads `snapshot.json` from disk every 1s (plus on
  focus/visibility change) and renders all four sections.

All writes to `snapshot.json` go through a single in-process mutex, so producers
never race each other. The full schema, write-coordination rules, and per-component
status live in [`.claude/architecture.md`](.claude/architecture.md) — read that before
changing any of this.

## Where runtime state lives

`%USERPROFILE%\.humcon\` — contains `snapshot.json`, `commands.jsonl`, and a `whisper\`
subdirectory. Override the base directory with `HUMCON_DIR`.

## Running it

From the project root:

```
cargo tauri dev
```

To just check the Rust side compiles (the crate is in `src-tauri/`, so plain
`cargo build` from the root fails):

```
cd src-tauri && cargo build
```

Requires Rust (MSVC toolchain), Node, `tauri-cli`, and WebView2.

The app is a native Windows process. If you launch it from a WSL shell, the whole
process tree dies the moment the launching WSL command returns — run it from a
Windows terminal, or hold the launching command open for the duration of the run.
See `.claude/architecture.md`'s Platform notes for details.

## One-time setup: whisper.cpp

Not fetched automatically. Into `%USERPROFILE%\.humcon\whisper\`:

1. From the [whisper.cpp releases](https://github.com/ggml-org/whisper.cpp/releases),
   download `whisper-bin-x64.zip` and extract `whisper-cli.exe` **plus its `whisper.dll`
   and every `ggml*.dll` sibling** — it's a DLL-dependent build and won't launch without them.
2. Download the model as `ggml-base.en.bin`.

Missing binary or model is a startup warning, not a crash — the hotkey still records,
but transcription reports the missing file. Override the paths with
`HUMCON_WHISPER_BIN` / `HUMCON_WHISPER_MODEL`.

## Summarization

Set `ANTHROPIC_API_KEY` in the Windows environment. A WSL-side `export` will not reach
the Windows process unless the variable is also listed in `WSLENV`. Without a key,
voice notes still transcribe fine — `voice_note.summary` just stays `null`.

## Shell command log hook

Source `hooks/humcon-log.sh` from `~/.bashrc` in WSL. It only logs interactive
commands, and skips anything that looks like it contains a secret (password, token,
API key, etc.) entirely rather than redacting it.

## Browser extension

Load-unpacked from `extension/` — see [`extension/README.md`](extension/README.md)
for install steps and what it does and doesn't report.

## More detail

For the frozen `snapshot.json` schema, full component status, session-by-session
history, and known limitations, see [`.claude/architecture.md`](.claude/architecture.md).
