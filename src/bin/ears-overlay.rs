//! Standalone GTK4 layer-shell overlay for eaRS dictation.
//!
//! This binary runs entirely locally with:
//! - Local Parakeet STT (ROCm accelerated)
//! - Remote Ollama for LLM correction (HTTP only)
//!
//! Usage:
//!   ears-overlay --ollama http://athena:11434

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "ears-overlay", about = "Local dictation overlay with remote LLM correction")]
struct Args {
    #[arg(
        long,
        env = "EARS_OLLAMA_URL",
        default_value = "http://localhost:11434",
        help = "Ollama API endpoint for LLM correction"
    )]
    ollama_url: String,

    #[arg(
        long,
        env = "EARS_OLLAMA_MODEL",
        default_value = "qwen2.5:7b",
        help = "Ollama model for text correction"
    )]
    ollama_model: String,

    #[arg(long, default_value = "400", help = "Overlay window width")]
    width: u32,

    #[arg(long, default_value = "200", help = "Overlay window height")]
    height: u32,

    #[arg(long, help = "Audio input device (default: system default)")]
    device: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    eprintln!("ears-overlay starting...");
    eprintln!("Ollama: {} ({})", args.ollama_url, args.ollama_model);
    eprintln!("STT: Local Parakeet with ROCm");

    // TODO: Initialize Parakeet STT engine
    // TODO: Initialize GTK4 and layer-shell overlay
    // TODO: Set up audio capture
    // TODO: Set up signal handlers
    // TODO: Run main loop

    Ok(())
}
