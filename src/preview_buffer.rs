//! Preview buffer state management for buffer-first dictation.
//!
//! Tracks words as they arrive from STT, manages committed sections,
//! and applies LLM corrections to the active buffer.

/// A section of text to display in the overlay
#[derive(Debug, Clone)]
pub struct DisplaySection {
    pub text: String,
    pub style: DisplayStyle,
}

/// Visual style for a display section
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStyle {
    /// Already committed/pasted - shown dimmed
    Committed,
    /// Active buffer - shown bright/normal
    Active,
    /// Separator line between sections
    Separator,
}

/// Buffer state for preview overlay mode
#[derive(Debug, Default)]
pub struct PreviewBuffer {
    /// Sections that have been checkpointed (pasted) - shown dimmed
    committed_sections: Vec<String>,
    /// Words in the current active buffer (not yet pasted)
    active_words: Vec<String>,
    /// LLM-corrected version of active buffer (if available)
    corrected_active: Option<String>,
    /// Whether a correction is currently in progress
    correction_pending: bool,
}

impl PreviewBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a new word to the active buffer
    pub fn add_word(&mut self, word: String) {
        self.active_words.push(word);
        // Invalidate any existing correction since content changed
        self.corrected_active = None;
    }

    /// Get the current active text (uncorrected)
    pub fn active_text(&self) -> String {
        self.active_words.join(" ")
    }

    /// Get the corrected active text, or uncorrected if no correction available
    pub fn active_text_display(&self) -> String {
        self.corrected_active
            .clone()
            .unwrap_or_else(|| self.active_text())
    }

    /// Apply an LLM correction to the active buffer (full replacement for final correction)
    pub fn apply_correction(&mut self, corrected: String) {
        self.corrected_active = Some(corrected);
        self.correction_pending = false;
    }

    /// Apply a chunk correction by splicing corrected words into active_words
    ///
    /// `start` is the index of the first word in the chunk within active_words.
    /// `original_len` is the number of words the chunk originally contained.
    /// Words added after the chunk (during correction) are preserved.
    pub fn apply_chunk_correction(&mut self, start: usize, original_len: usize, corrected: &str) {
        let corrected_words: Vec<String> = corrected.split_whitespace().map(String::from).collect();
        let end = (start + original_len).min(self.active_words.len());
        if start <= end && start <= self.active_words.len() {
            self.active_words.splice(start..end, corrected_words);
        }
        // Clear corrected_active — active_words now reflects corrections directly
        self.corrected_active = None;
    }

    /// Mark that a correction is in progress
    pub fn set_correction_pending(&mut self, pending: bool) {
        self.correction_pending = pending;
    }

    /// Check if a correction is in progress
    pub fn is_correction_pending(&self) -> bool {
        self.correction_pending
    }

    /// Get the number of words in the active buffer
    pub fn active_word_count(&self) -> usize {
        self.active_words.len()
    }

    /// Checkpoint: get text to paste, move active to committed, return the text
    pub fn checkpoint(&mut self) -> Option<String> {
        if self.active_words.is_empty() {
            return None;
        }

        // Use corrected text if available, otherwise raw
        let text = self.active_text_display();

        // Move to committed
        self.committed_sections.push(text.clone());

        // Clear active buffer
        self.active_words.clear();
        self.corrected_active = None;
        self.correction_pending = false;

        Some(text)
    }

    /// Complete a breakpoint: push corrected text to committed, preserve post-breakpoint words.
    /// `boundary` = number of active_words that existed when breakpoint fired.
    /// Words added after the breakpoint (during correction) are preserved in active_words.
    pub fn breakpoint_complete(&mut self, corrected: String, boundary: usize) -> Option<String> {
        if corrected.is_empty() && boundary == 0 {
            return None;
        }
        self.committed_sections.push(corrected.clone());
        let actual_boundary = boundary.min(self.active_words.len());
        self.active_words.drain(..actual_boundary);
        self.corrected_active = None;
        self.correction_pending = false;
        Some(corrected)
    }

    /// Commit: get all text (committed + active), clear everything
    pub fn commit(&mut self) -> Option<String> {
        let mut parts = Vec::new();

        // Add all committed sections
        for section in &self.committed_sections {
            parts.push(section.clone());
        }

        // Add active section if not empty
        if !self.active_words.is_empty() {
            parts.push(self.active_text_display());
        }

        if parts.is_empty() {
            return None;
        }

        // Clear everything
        self.committed_sections.clear();
        self.active_words.clear();
        self.corrected_active = None;
        self.correction_pending = false;

        // Join with newlines (each checkpoint section becomes a paragraph)
        Some(parts.join("\n\n"))
    }

    /// Clear everything without returning text
    pub fn clear(&mut self) {
        self.committed_sections.clear();
        self.active_words.clear();
        self.corrected_active = None;
        self.correction_pending = false;
    }

    /// Check if the buffer is completely empty
    pub fn is_empty(&self) -> bool {
        self.committed_sections.is_empty() && self.active_words.is_empty()
    }

    /// Get display sections for rendering in the overlay
    pub fn display_sections(&self) -> Vec<DisplaySection> {
        let mut sections = Vec::new();

        // Add committed sections with separators
        for (i, text) in self.committed_sections.iter().enumerate() {
            if i > 0 {
                sections.push(DisplaySection {
                    text: String::new(),
                    style: DisplayStyle::Separator,
                });
            }
            sections.push(DisplaySection {
                text: text.clone(),
                style: DisplayStyle::Committed,
            });
        }

        // Add separator before active if there are committed sections
        if !self.committed_sections.is_empty() && !self.active_words.is_empty() {
            sections.push(DisplaySection {
                text: String::new(),
                style: DisplayStyle::Separator,
            });
        }

        // Add active section
        if !self.active_words.is_empty() {
            sections.push(DisplaySection {
                text: self.active_text_display(),
                style: DisplayStyle::Active,
            });
        }

        sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_words() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("hello".to_string());
        buffer.add_word("world".to_string());
        assert_eq!(buffer.active_text(), "hello world");
        assert_eq!(buffer.active_word_count(), 2);
    }

    #[test]
    fn test_correction() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("helo".to_string());
        buffer.add_word("wrold".to_string());
        buffer.apply_correction("hello world".to_string());
        assert_eq!(buffer.active_text_display(), "hello world");
        assert_eq!(buffer.active_text(), "helo wrold"); // original preserved
    }

    #[test]
    fn test_checkpoint() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("first".to_string());
        buffer.add_word("section".to_string());

        let text = buffer.checkpoint();
        assert_eq!(text, Some("first section".to_string()));
        assert!(buffer.active_words.is_empty());
        assert_eq!(buffer.committed_sections.len(), 1);

        buffer.add_word("second".to_string());
        buffer.add_word("section".to_string());

        let sections = buffer.display_sections();
        assert_eq!(sections.len(), 3); // committed, separator, active
    }

    #[test]
    fn test_commit() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("first".to_string());
        buffer.checkpoint();
        buffer.add_word("second".to_string());

        let text = buffer.commit();
        assert_eq!(text, Some("first\n\nsecond".to_string()));
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_chunk_correction_no_new_words() {
        let mut buffer = PreviewBuffer::new();
        for w in ["hello", "wrold", "foo", "bar"] {
            buffer.add_word(w.to_string());
        }
        // Correct last 2 words (chunk at index 2, len 2)
        buffer.apply_chunk_correction(2, 2, "foo bar");
        assert_eq!(buffer.active_text(), "hello wrold foo bar");
    }

    #[test]
    fn test_chunk_correction_with_new_words() {
        let mut buffer = PreviewBuffer::new();
        // Original 4 words
        for w in ["hello", "wrold", "foo", "bar"] {
            buffer.add_word(w.to_string());
        }
        // Simulate: chunk was last 2 words, but 2 new words arrived during correction
        buffer.add_word("baz".to_string());
        buffer.add_word("qux".to_string());
        // Now active_words = [hello, wrold, foo, bar, baz, qux]
        // Correct chunk at index 2 (foo, bar) → "Foo Bar"
        buffer.apply_chunk_correction(2, 2, "Foo Bar");
        // Should preserve baz, qux after the corrected chunk
        assert_eq!(buffer.active_text(), "hello wrold Foo Bar baz qux");
    }

    #[test]
    fn test_chunk_correction_changes_word_count() {
        let mut buffer = PreviewBuffer::new();
        for w in ["i", "can", "not", "go"] {
            buffer.add_word(w.to_string());
        }
        // Correct "can not" (index 1, len 2) → "cannot" (1 word)
        buffer.apply_chunk_correction(1, 2, "cannot");
        assert_eq!(buffer.active_text(), "i cannot go");
        assert_eq!(buffer.active_word_count(), 3);
    }

    #[test]
    fn test_breakpoint_complete_preserves_new_words() {
        let mut buffer = PreviewBuffer::new();
        for w in ["alpha", "beta", "gamma", "delta"] {
            buffer.add_word(w.to_string());
        }
        // Simulate new words arriving during correction
        buffer.add_word("epsilon".to_string());
        buffer.add_word("zeta".to_string());

        let result = buffer.breakpoint_complete("alpha beta gamma delta".to_string(), 4);
        assert_eq!(result, Some("alpha beta gamma delta".to_string()));
        assert_eq!(buffer.active_text(), "epsilon zeta");
        assert_eq!(buffer.committed_sections.len(), 1);
    }

    #[test]
    fn test_breakpoint_complete_empty_buffer() {
        let mut buffer = PreviewBuffer::new();
        let result = buffer.breakpoint_complete(String::new(), 0);
        assert_eq!(result, None);
        assert!(buffer.active_words.is_empty());
        assert!(buffer.committed_sections.is_empty());
    }

    #[test]
    fn test_breakpoint_complete_boundary_exceeds_len() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("one".to_string());
        buffer.add_word("two".to_string());
        let result = buffer.breakpoint_complete("one two".to_string(), 5);
        assert_eq!(result, Some("one two".to_string()));
        assert!(buffer.active_words.is_empty());
        assert_eq!(buffer.committed_sections.len(), 1);
    }

    #[test]
    fn test_breakpoint_complete_zero_boundary() {
        let mut buffer = PreviewBuffer::new();
        buffer.add_word("keep".to_string());
        buffer.add_word("these".to_string());
        let result = buffer.breakpoint_complete("committed".to_string(), 0);
        assert_eq!(result, Some("committed".to_string()));
        assert_eq!(buffer.active_text(), "keep these");
        assert_eq!(buffer.committed_sections.len(), 1);
    }
}
