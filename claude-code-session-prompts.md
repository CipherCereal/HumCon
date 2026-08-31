# Flow-Context — Claude Code Session Prompts

How to use this file:
- One prompt per fresh Claude Code session (`/clear` or new `claude` session between each).
- Paste the whole block as your *first* message in that session — don't add anything before it.
- Each prompt ends with an explicit plan-mode trigger. Let Claude propose its plan, review it, then approve.
- After Claude finishes, run `/done-check`, check `/usage`, update `architecture.md` if the auto-update was thin, then move to the next prompt.
- Skip Session 6 (browser extension) without guilt if you're low on credit — it's independent of everything else.

---

## Session 0 — Finalize the snapshot.json schema

```xml
<role>
You are a senior systems architect reviewing a data contract before any code depends on it.
</role>

<context>
I'm building Flow-Context, a Windows desktop tool (Tauri + Rust backend) that captures active window info, terminal commands, a voice-note summary, and browser tab info into a single local file, snapshot.json, then renders it as a "resume card." Every other component I build after this session will read or write this file, so getting the shape right now avoids expensive rework later. Read CLAUDE.md and architecture.md before doing anything else — architecture.md has a proposed schema and a list of open questions under "snapshot.json schema."
</context>

<task>
Review the proposed schema and resolve the open questions listed in architecture.md. You are not writing any implementation code in this session — only finalizing the data contract.
</task>

<process>
1. Read CLAUDE.md and architecture.md in full.
2. Evaluate the proposed schema for correctness given the four data sources (window capture, shell commands, voice note, browser tab).
3. Answer each open question explicitly, with a one-line reason for the decision.
4. Propose any changes to the schema needed as a result.
5. Update architecture.md yourself: mark the schema section as FROZEN, write the final schema, and record the resolved decisions in the session log section.
</process>

<constraints>
- Do not write any Rust, TS, or shell code in this session.
- Do not touch any files other than architecture.md.
- If you think the proposed schema has a structural problem, say so plainly before proposing a fix — don't silently patch around it.
</constraints>

<definition_of_done>
architecture.md's schema section is marked FROZEN, every open question has a recorded decision, and the session log has a new entry for this session.
</definition_of_done>

Before making any changes, propose your plan and wait for my approval.
```

---

## Session 1 — Rust window/app capture

```xml
<role>
You are a Rust systems engineer experienced with Windows-specific process and window APIs.
</role>

<context>
Read CLAUDE.md and architecture.md — the snapshot.json schema is now frozen there. This app is Windows-only, dev shell is WSL bash, but this Tauri app itself runs as a native Windows process, not under WSL. I'm new to Rust and reviewing your work rather than writing it myself, so favor clear, well-commented code over cleverness.
</context>

<task>
Implement window/app capture using the active-win-pos-rs crate, polling the active window every few seconds and writing the result into snapshot.json's active_window field, exactly matching the frozen schema.
</task>

<process>
1. Before writing implementation code, use a subagent to research how active-win-pos-rs behaves specifically on Windows: window title edge cases (empty, non-UTF8), behavior when there's no active window (e.g. desktop focused, screen locked), and any known issues. Summarize findings before proceeding.
2. Propose your implementation plan based on those findings.
3. Implement polling and the write to snapshot.json, only touching this component's key in the file.
4. Run it for a few minutes with real usage (switching between real apps) to confirm it behaves correctly, not just that it compiles.
```

<constraints>
- Only write to the active_window key in snapshot.json — do not touch other keys.
- Handle the no-active-window case explicitly rather than letting it panic or write garbage.
- Do not change the frozen schema. If you believe it needs to change, stop and flag it instead of proceeding.
</constraints>

<definition_of_done>
Compiles cleanly, handles no-active-window and odd-title cases without crashing, has been run continuously for several minutes against real window switches, and architecture.md's component table and session log are updated.
</definition_of_done>

Before writing any code, propose your plan (including subagent findings) and wait for my approval.
```

---

## Session 2 — Shell command log hook

```xml
<role>
You are a shell scripting specialist experienced with WSL-to-Windows interop.
</role>

<context>
Read CLAUDE.md and architecture.md. Dev shell is WSL bash. The Tauri app that will read this log runs as a native Windows process, so anything WSL writes must land on a Windows-visible path (/mnt/c/...), never a WSL-only path.
</context>

<task>
Add a bash hook that appends the last N shell commands (default 20, per the frozen schema decision) to a log file the Tauri app can read, matching the recent_commands structure in snapshot.json.
</task>

<process>
1. Confirm the exact /mnt/c/... path to use, consistent with wherever the Tauri app expects to read from — check the Session 1 code if it already references a config path.
2. Implement the hook (likely via a PROMPT_COMMAND or trap-based approach in bash).
3. Decide and implement how this hook's output gets merged into snapshot.json's recent_commands key — either the hook writes directly, or it writes a raw log another small piece of code parses. State which approach you're using and why.
4. Test by running several real commands and confirming they appear correctly, in order, capped at N entries.
</process>

<constraints>
- Must write through a /mnt/c/... path — flag immediately if you're about to do otherwise.
- Only touch the recent_commands key.
- Don't capture sensitive-looking commands (anything that looks like it contains a password or API key inline) — skip logging those rather than trying to redact them.
</constraints>

<definition_of_done>
Real commands run in WSL show up correctly and in order in snapshot.json's recent_commands, capped at N, with no WSL-only paths involved. architecture.md updated.
</definition_of_done>

Before writing any code, propose your plan and wait for my approval.
```

---

## Session 3 — Claude summarization call

```xml
<role>
You are an API integration engineer who treats external calls as things that will eventually fail and designs for that from the start.
</role>

<context>
Read CLAUDE.md and architecture.md. This component takes a transcript string (from whisper.cpp, built in a later session) and sends it to the Claude Haiku API for a short summary, then writes the result into snapshot.json's voice_note.summary field. Build this now as a standalone module that takes a transcript string as input, independent of whisper.cpp, so it can be tested with fake transcripts before the real audio pipeline exists.
</context>

<task>
Implement the Claude Haiku summarization call as an isolated, testable module.
</task>

<process>
1. Propose the function signature and how it will be invoked once whisper.cpp exists later.
2. Implement the API call, using a short, clear prompt to Claude Haiku asking for a brief summary suited to a "resume card" (a sentence or two capturing what the person said they were about to do or working on).
3. Handle failure cases explicitly: network failure, empty transcript, API error response — in each case, decide what gets written to voice_note.summary (e.g. null with an error noted elsewhere) rather than letting a failure take down the rest of the snapshot write.
4. Test with 3-4 realistic fake transcripts you write yourself, covering a normal case, a very short transcript, and an empty transcript.
</process>

<constraints>
- Never let a failure in this module prevent other snapshot.json fields from being written.
- Only touch the voice_note.summary key (and voice_note.recorded_at).
- Keep the Haiku prompt itself short — this is a cheap, frequent call, don't over-engineer it.
</constraints>

<definition_of_done>
Handles all three failure cases without crashing or corrupting snapshot.json, produces sensible summaries for the test transcripts, and architecture.md is updated including the function signature whisper.cpp will need to call.
</definition_of_done>

Before writing any code, propose your plan and wait for my approval.
```

---

## Session 4 — whisper.cpp integration (highest risk — watch this one live)

```xml
<role>
You are an audio pipeline engineer experienced with local speech-to-text on Windows, and realistic about where native-binary integrations tend to break.
</role>

<context>
Read CLAUDE.md and architecture.md, including the function signature the Session 3 summarization module exposes. This is the highest-risk component in the whole build — if it doesn't work cleanly within a reasonable number of attempts, I'd rather have a clearly-documented stub than a half-working mess.
</context>

<task>
Wire a global hotkey to start/stop mic recording, run the recording through whisper.cpp for local transcription, and pass the resulting transcript into the Session 3 summarization module.
</task>

<process>
1. Propose your approach, including how whisper.cpp will be invoked from Rust (bundled binary, subprocess call, or bindings) and flag any Windows-specific packaging concerns up front.
2. Implement the hotkey listener.
3. Implement the recording-to-whisper.cpp pipeline.
4. Connect the transcript output to the Session 3 module.
5. Test with real speech end to end.
</process>

<constraints>
- If you hit the same blocking error more than twice after distinct fix attempts, stop trying variations. Instead, tell me what's blocking it and implement a clearly-labeled stub (a function with the same signature that returns a fixed test transcript) so the rest of the pipeline stays usable, and record the real issue in architecture.md's known limitations.
- Do not silently leave this half-working in a way that looks complete — either it works end-to-end with real audio, or it's an explicit, documented stub.
</constraints>

<definition_of_done>
Either: real speech produces a real transcript that reaches the summarization module end-to-end — OR — a clearly documented stub is in place with the real blocker written into architecture.md's known limitations, not buried in code comments.
</definition_of_done>

Before writing any code, propose your plan and wait for my approval.
```

---

## Session 5 — Resume card UI

```xml
<role>
You are a Tauri frontend developer who prioritizes a working, legible interface over visual polish for a first pass.
</role>

<context>
Read CLAUDE.md and architecture.md. The snapshot.json schema is frozen and every field-producing component should now exist (window capture, commands, voice summary, and possibly browser tab if Session 6 is done). Build the window that reads this file and displays it as a "resume card."
</context>

<task>
Build a Tauri window that reads snapshot.json and renders active window, recent commands, voice summary, and browser tab (if present) in a clear, readable layout.
</task>

<process>
1. Propose the layout and how/when the window reads the file (on open, on a poll interval, or on a file-watch trigger).
2. Implement the read and render logic.
3. Handle missing/null fields gracefully (e.g. no voice note yet) rather than showing blank or broken UI.
4. Test by manually editing snapshot.json to include and omit each field, confirming the UI handles both.
</process>

<constraints>
- Read-only against snapshot.json — this component never writes to it.
- Don't block on file reads in a way that freezes the window if the file is mid-write elsewhere.
</constraints>

<definition_of_done>
Renders correctly against both a fully-populated snapshot.json and one with missing fields, without crashing or showing broken layout. architecture.md updated.
</definition_of_done>

Before writing any code, propose your plan and wait for my approval.
```

---

## Session 6 — Browser extension (optional — skip if low on credit)

```xml
<role>
You are a Chrome extension developer building a minimal MV3 extension.
</role>

<context>
Read CLAUDE.md and architecture.md. The Tauri app runs a small local HTTP server. This extension's only job is reporting the active tab's URL and title to that server, which writes it into snapshot.json's browser_tab key.
</context>

<task>
Build a minimal MV3 Chrome extension that pings the local Tauri server with the active tab's URL and title whenever the active tab changes.
</task>

<process>
1. Propose the extension's structure (manifest, background script, permissions needed) and confirm the local server endpoint/contract with what Session 1-5 code already expects, if anything does.
2. Implement it.
3. Test manually by switching tabs and confirming snapshot.json's browser_tab field updates.
</process>

<constraints>
- Request the minimum permissions needed — don't request broad host permissions if activeTab-style access suffices.
- Only touch the browser_tab key.
</constraints>

<definition_of_done>
Switching Chrome tabs updates browser_tab in snapshot.json correctly, with minimal permissions requested. architecture.md updated.
</definition_of_done>

Before writing any code, propose your plan and wait for my approval.
```

---

## Final Session — Wrap-up / handoff status pass

```xml
<role>
You are a technical lead doing a handoff audit before stepping away from a project for an unknown length of time.
</role>

<context>
Read CLAUDE.md and architecture.md in full, then look at the actual code for every component listed in the component status table.
</context>

<task>
Produce an honest, complete status audit so this project can be picked back up later without re-reading all the code from scratch.
</task>

<process>
1. For every component, verify against the actual code (not just what architecture.md claims) whether it's solid, stubbed, or untested.
2. Update the component status table to reflect reality.
3. For every incomplete or stubbed component, write a concrete, specific next step — not "finish this," but the actual first action to take.
4. Update known limitations with anything discovered during this audit that wasn't already recorded.
5. Do not fix anything in this session — this is audit-only, to avoid unplanned spend right when credit is tightest.
</process>

<constraints>
- Do not write or modify any code in this session, only architecture.md.
- Be honest about partial or shaky work — this document is only useful if it's accurate.
</constraints>

<definition_of_done>
architecture.md's component table matches what the code actually does, every incomplete item has a specific next step, and known limitations are complete.
</definition_of_done>
```
