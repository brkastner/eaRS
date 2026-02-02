//! LLM-based text correction using Ollama or compatible API.
//!
//! Accumulates transcribed words into sentences, sends completed sentences
//! to a local LLM for grammar/punctuation correction.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for LLM correction
#[derive(Debug, Clone)]
pub struct LlmCorrectConfig {
    /// Ollama API endpoint (default: http://localhost:11434)
    pub endpoint: String,
    /// Model to use for fast, live correction (default: qwen2.5:14b)
    pub model: String,
    /// Model to use for final paragraph correction (default: same as model)
    pub final_model: String,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Max tokens for fast correction
    pub num_predict_fast: i32,
    /// Max tokens for final correction
    pub num_predict_final: i32,
    /// Sampling temperature
    pub temperature: f32,
}

impl Default for LlmCorrectConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5:14b".to_string(),
            final_model: "qwen2.5:14b".to_string(),
            timeout_secs: 10,
            num_predict_fast: 128,
            num_predict_final: 512,
            temperature: 0.1,
        }
    }
}

#[derive(Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_predict: i32,
}

#[derive(Deserialize)]
struct OllamaResponse {
    response: String,
}

/// Sentence corrector that accumulates words and corrects complete sentences
pub struct SentenceCorrector {
    client: Client,
    config: LlmCorrectConfig,
    buffer: Vec<String>,
    /// Words that have been typed but not yet corrected
    typed_count: usize,
}

impl SentenceCorrector {
    pub fn new(config: LlmCorrectConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("failed to create HTTP client")?;

        Ok(Self {
            client,
            config,
            buffer: Vec::new(),
            typed_count: 0,
        })
    }

    /// Add a word to the buffer.
    /// Returns (should_type_word, Option<correction>) where correction contains
    /// (backspace_count, corrected_text) if a sentence was completed and corrected.
    pub async fn add_word(&mut self, word: &str) -> Result<(bool, Option<(usize, String)>)> {
        self.buffer.push(word.to_string());

        // Check for sentence boundary (period, question mark, exclamation)
        let trimmed = word.trim();
        let is_sentence_end = trimmed.ends_with('.')
            || trimmed.ends_with('?')
            || trimmed.ends_with('!');

        if is_sentence_end && self.buffer.len() >= 2 {
            // We have a complete sentence
            let original = self.buffer.join(" ");
            let words_to_backspace = self.typed_count;

            // Correct the sentence
            let corrected = self.correct_sentence(&original).await?;

            // Reset buffer and typed count
            self.buffer.clear();
            self.typed_count = 0;

            // If correction is different, return backspace count and new text
            if corrected != original {
                // +1 for each space after words (words_to_backspace spaces)
                // Plus the characters in each word
                Ok((false, Some((words_to_backspace, corrected))))
            } else {
                // No correction needed, just type the final word
                self.typed_count += 1;
                Ok((true, None))
            }
        } else {
            // Not end of sentence, type the word
            self.typed_count += 1;
            Ok((true, None))
        }
    }

    /// Flush any remaining words in the buffer (for end of stream)
    pub async fn flush(&mut self) -> Result<Option<(usize, String)>> {
        if self.buffer.is_empty() {
            return Ok(None);
        }

        let original = self.buffer.join(" ");
        let words_to_backspace = self.typed_count;

        let corrected = self.correct_sentence(&original).await?;

        self.buffer.clear();
        self.typed_count = 0;

        if corrected != original {
            Ok(Some((words_to_backspace, corrected)))
        } else {
            Ok(None)
        }
    }

    /// Send a chunk to the LLM for correction
    pub async fn correct_sentence(&self, sentence: &str) -> Result<String> {
        let prompt = format!(
            r#"Fix transcription errors and grammar in this dictated text. Common STT errors: "bath"→"batch", "B4"→"before", "uh"→remove. Preserve code identifiers, file paths, flags, and casing. Preserve all line breaks exactly. Output ONLY the corrected text, nothing else.

Text: {}"#,
            sentence
        );

        self.call_ollama(&prompt, &self.config.model, self.config.num_predict_fast)
            .await
    }

    /// Final paragraph correction with more thorough cleanup
    pub async fn correct_paragraph(&self, paragraph: &str) -> Result<String> {
        let prompt = format!(
            r#"Clean up this dictated paragraph. Fix:
- Transcription errors (bath→batch, B4→before)
- Grammar and punctuation
- Remove filler words (uh, um)
- Fix sentence boundaries
IMPORTANT: Preserve code identifiers, file paths, flags, and casing. Preserve all line breaks and paragraph structure exactly.
Output ONLY the corrected text, preserving meaning.

Text: {}"#,
            paragraph
        );

        self.call_ollama(
            &prompt,
            &self.config.final_model,
            self.config.num_predict_final,
        )
        .await
    }

    async fn call_ollama(&self, prompt: &str, model: &str, num_predict: i32) -> Result<String> {
        let request = OllamaRequest {
            model,
            prompt: &prompt,
            stream: false,
            options: OllamaOptions {
                temperature: self.config.temperature,
                num_predict,
            },
        };

        let url = format!("{}/api/generate", self.config.endpoint);

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("failed to send request to Ollama")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama error {}: {}", status, body);
        }

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .context("failed to parse Ollama response")?;

        // Clean up response
        let corrected = ollama_response
            .response
            .trim()
            .trim_matches('"')
            .to_string();

        Ok(corrected)
    }
}

/// Check if Ollama is available at the given endpoint
pub async fn check_ollama_available(endpoint: &str) -> bool {
    let client = match Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let url = format!("{}/api/tags", endpoint);
    client.get(&url).send().await.is_ok()
}
