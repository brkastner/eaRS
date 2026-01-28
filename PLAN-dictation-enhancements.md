# eaRS Dictation Enhancement Plan

## Overview

Two enhancements to improve the LLM-corrected dictation experience:
1. **Pause-triggered final correction** — Clean up full paragraph when user stops speaking
2. **Waybar status integration** — Visual feedback for dictation state

## Current State

- Chunked LLM correction triggers every 6 words or on punctuation
- No final paragraph cleanup when dictation pauses
- No visual indicator of dictation state (listening/paused/processing)
- Code location: `src/bin/ears-dictation.rs`, `src/llm_correct.rs`

## Feature 1: Pause-Triggered Final Correction

### Goal
When no words are received for 5+ seconds, send the entire accumulated paragraph to the LLM for a thorough cleanup pass. This fixes:
- Chunk boundary artifacts ("blue Everything built using blueprints")
- Capitalization errors from chunk splits
- Overall coherence issues

### Implementation

1. **Track last word timestamp** in the main message loop
2. **Add timeout check** — if 5 seconds pass without a word while capturing:
   - Call `correct_final_paragraph()` (already implemented, just not wired up)
   - Reset the paragraph buffer
3. **Integrate with capture toggle** — also trigger final correction when user toggles off

### Files to Modify
- `src/bin/ears-dictation.rs` — add timestamp tracking and timeout logic in main loop

### Technical Notes
- Use `tokio::time::Instant` for tracking
- Check timeout in the `select!` loop's default branch
- The `correct_final_paragraph()` function already exists and handles backspace+retype

## Feature 2: Waybar Status Integration

### Goal
Show dictation state in waybar with icons:
- 🎤 Listening (actively transcribing)
- ⏸ Paused (capture off)
- 🔄 Processing (LLM correction in progress)

### Implementation

1. **State file** at `~/.local/state/ears/status.json`:
   ```json
   {"state": "listening", "last_update": 1706500000}
   ```

2. **Update state** in ears-dictation:
   - On capture toggle (listening ↔ paused)
   - Before/after LLM calls (processing)

3. **Waybar custom module** config:
   ```json
   "custom/ears": {
     "exec": "cat ~/.local/state/ears/status.json | jq -r '.state' | sed 's/listening/🎤/;s/paused/⏸/;s/processing/🔄/'",
     "interval": 1,
     "format": "{}"
   }
   ```

### Files to Modify
- `src/bin/ears-dictation.rs` — add state file writes
- User's waybar config (chezmoi)

### Technical Notes
- Use atomic writes (write to temp, rename) to avoid partial reads
- Consider adding word count or last transcription snippet to status
- Optional: desktop notifications on state change (already have `notifica` crate)

## Testing Plan

1. Test pause detection with various pause durations (3s, 5s, 10s)
2. Verify final correction improves output quality
3. Confirm waybar updates in real-time
4. Test edge cases: rapid toggle, very long paragraphs, LLM timeout

## Open Questions

- Should pause duration be configurable via CLI/env?
- Should final correction be optional (some users may not want the delay)?
- Waybar tooltip with more details (word count, last correction)?
