# Overlay Bugs and Remaining Work

Post-testing issues and remaining implementation tasks. Each section is independent and can be worked on in parallel sessions.

Read `docs/fork-context.md` for full environment and architecture context.

## Bug 1: Text loss during LLM correction processing

**Status:** Open
**Severity:** High
**File:** `src/bin/ears-dictation.rs` (preview mode pause detection, ~line 803)

**Problem:** When the user speaks while Ollama is processing a chunk correction (~1.5s pause trigger), words arriving during that async correction window are lost or garbled. The correction replaces the chunk text, but new words that arrived between `take_chunk()` and the correction response aren't accounted for.

**Repro:** Speak continuously, pause briefly (triggers chunk correction), immediately resume speaking. Words spoken during correction processing are dropped.

**Fix approach:** Buffer words that arrive during correction. After correction completes, append the buffered words. The `CorrectionBuffer` may need a "pending during correction" state, or the overlay command handler needs to queue words received during the correction await.

**Key code path:**
```
pause detected → take_chunk() → set_status(Correcting) → corrector.correct_sentence().await → send_correction()
                                 ↑ words arriving here are lost
```

## Bug 2: Text loss on checkpoint

**Status:** Open
**Severity:** Medium
**File:** `src/bin/ears-dictation.rs` (checkpoint handler, ~line 610)

**Problem:** When checkpoint is triggered (SIGUSR2 / numpad -), some buffered text may not be included in the paste.

**Repro:** Dictate several sentences, trigger checkpoint. Some of the last words may be missing from the pasted text.

**Fix approach:** Ensure the preview buffer's `checkpoint()` method captures all accumulated text including any in-flight words. May need to drain the websocket briefly before checkpointing (similar to the toggle-off drain pattern).

## Bug 3: Overlay won't close on toggle-off (FIXED - needs re-test)

**Status:** Fix pushed (commit 53423b16), needs re-test
**File:** `src/bin/ears-dictation.rs` (final_correct_rx handler, ~line 652)

**Problem:** Toggle-off (SIGUSR1) didn't send Commit to overlay. Overlay stayed open indefinitely.

**Fix:** Added preview-mode branch in `final_correct_rx` handler that drains words to overlay, runs final correction through overlay, then commits (paste + close).

## Task 12: Update CLAUDE.md documentation

**Status:** Pending (from gtk4-layer-shell plan)
**Files:** `CLAUDE.md`

Update with:
- `preview-overlay` feature now uses gtk4-layer-shell (not egui)
- `gtk_overlay.rs` in key files list
- Overlay feature flag reference
- `ears-overlay` binary description

## Task 13: Create ears-overlay systemd service

**Status:** Pending (from gtk4-layer-shell plan)
**File:** `contrib/systemd/ears-overlay.service`

Create systemd user service for the standalone `ears-overlay` binary. Similar to `ears-dictation-remote.service` but uses local Parakeet STT instead of connecting to a remote server.

## Task 14: Wire up ears-overlay binary

**Status:** Pending (from gtk4-layer-shell plan)
**File:** `src/bin/ears-overlay.rs`
**Depends on:** Tasks 12, 13

Currently a skeleton with TODOs. Needs:
- Local Parakeet STT engine initialization
- Audio capture via cpal
- GTK4 overlay spawning (reuse `gtk_overlay::spawn_overlay`)
- Remote Ollama correction (reuse `llm_correct::SentenceCorrector`)
- Signal handling (SIGUSR1 toggle, SIGUSR2 checkpoint)
- Main loop wiring all components together

Pattern: copy flow from `ears-dictation.rs` but replace WebSocket client with local Parakeet inference.

## Future: Hide/show daemon mode

**Status:** Design only (not started)

Instead of destroy/create overlay on each toggle cycle, keep GTK thread alive and toggle `window.set_visible()`. Reduces toggle latency.

Key changes:
- `app.hold()` to prevent GTK quit on window hide
- New `OverlayCommand::Hide` / `OverlayCommand::Show`
- `OverlayHandle` persists across toggle cycles
- Buffer resets on Show

## Future: External editor integration

**Status:** Design only (not started)

Open `$EDITOR --wait` with buffer contents for editing before commit. Like Claude Code's Ctrl+E pattern.

Key changes:
- New `OverlayCommand::Edit`
- Pause STT on edit trigger
- Write buffer to temp file, spawn editor, read back on close
- Commit edited text
