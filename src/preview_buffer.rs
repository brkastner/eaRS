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

    /// Apply an LLM correction to the active buffer
    pub fn apply_correction(&mut self, corrected: String) {
        self.corrected_active = Some(corrected);
        self.correction_pending = false;
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
}
