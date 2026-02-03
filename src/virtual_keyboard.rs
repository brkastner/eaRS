//! Virtual keyboard abstraction for cross-platform keyboard input injection.
//!
//! Supports:
//! - Linux (Wayland/X11): uinput kernel interface
//! - Other platforms: enigo fallback

use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

#[cfg(target_os = "linux")]
use uinput::{event::keyboard, Device};

use enigo::{Direction, Enigo, Keyboard, Settings};

/// Cross-platform virtual keyboard trait
pub trait VirtualKeyboard {
    /// Type text into the focused application
    fn type_text(&mut self, text: &str) -> Result<()>;

    /// Press and release a special key
    fn press_key(&mut self, key: SpecialKey) -> Result<()>;

    /// Delete N words using Ctrl+Backspace (faster than char-by-char)
    fn delete_words(&mut self, count: usize) -> Result<()>;

    /// Press a modifier key (hold it down)
    fn press_modifier(&mut self, modifier: Modifier) -> Result<()>;

    /// Release a modifier key
    fn release_modifier(&mut self, modifier: Modifier) -> Result<()>;

    /// Send a key chord (modifiers + character)
    fn send_chord(&mut self, modifiers: &[Modifier], ch: char) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct KeyboardTiming {
    pub char_delay: Duration,
    pub key_delay: Duration,
    pub backspace_delay: Duration,
    pub delete_word_delay: Duration,
    pub chord_delay: Duration,
    pub newline_mode: NewlineMode,
}

impl Default for KeyboardTiming {
    fn default() -> Self {
        Self {
            char_delay: Duration::from_micros(500),
            key_delay: Duration::from_micros(500),
            backspace_delay: Duration::from_millis(15),
            delete_word_delay: Duration::from_millis(10),
            chord_delay: Duration::from_millis(5),
            newline_mode: NewlineMode::Enter,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineMode {
    Enter,
    ShiftEnter,
}

/// Special keys that can be pressed
#[derive(Debug, Clone, Copy)]
pub enum SpecialKey {
    Enter,
    Backspace,
    Delete,
    Tab,
    Space,
    Escape,
    Left,
    Right,
    Up,
    Down,
}

/// Modifier keys for key chords
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Super,
}

/// Create the appropriate keyboard implementation for the current platform
pub fn create_virtual_keyboard() -> Result<Box<dyn VirtualKeyboard>> {
    create_virtual_keyboard_with_timing(KeyboardTiming::default())
}

pub fn create_virtual_keyboard_with_timing(
    timing: KeyboardTiming,
) -> Result<Box<dyn VirtualKeyboard>> {
    #[cfg(target_os = "linux")]
    {
        UInputKeyboard::new(timing)
            .map(|kb| Box::new(kb) as Box<dyn VirtualKeyboard>)
            .or_else(|e| {
                eprintln!("Warning: Failed to create uinput keyboard: {}", e);
                eprintln!("Falling back to enigo (may not work properly on Wayland)");
                Ok(Box::new(EnigoKeyboard::new(timing)?) as Box<dyn VirtualKeyboard>)
            })
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(Box::new(EnigoKeyboard::new(timing)?))
    }
}

// ============================================================================
// Linux uinput Implementation
// ============================================================================

#[cfg(target_os = "linux")]
pub struct UInputKeyboard {
    device: Device,
    timing: KeyboardTiming,
}

#[cfg(target_os = "linux")]
impl UInputKeyboard {
    pub fn new(timing: KeyboardTiming) -> Result<Self> {
        // Try to open /dev/uinput
        let device = uinput::open("/dev/uinput")
            .context(
                "Failed to open /dev/uinput. Please ensure:\n\
                      1. You are in the 'input' group: sudo usermod -a -G input $USER\n\
                      2. The uinput module is loaded: sudo modprobe uinput\n\
                      3. You have logged out and back in after adding to group",
            )?
            .name("eaRS Virtual Keyboard")?
            .event(keyboard::Keyboard::All)?
            .create()
            .context("Failed to create uinput device")?;

        Ok(Self { device, timing })
    }

    fn type_char(&mut self, ch: char) -> Result<()> {
        let needs_shift = ch.is_ascii_uppercase()
            || matches!(
                ch,
                '!' | '@'
                    | '#'
                    | '$'
                    | '%'
                    | '^'
                    | '&'
                    | '*'
                    | '('
                    | ')'
                    | '_'
                    | '+'
                    | '{'
                    | '}'
                    | '|'
                    | ':'
                    | '"'
                    | '<'
                    | '>'
                    | '?'
            );

        let key = char_to_key(ch)?;

        if needs_shift {
            self.device.press(&keyboard::Key::LeftShift)?;
        }

        self.device.click(&key)?;

        if needs_shift {
            self.device.release(&keyboard::Key::LeftShift)?;
        }

        self.device.synchronize()?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl VirtualKeyboard for UInputKeyboard {
    fn type_text(&mut self, text: &str) -> Result<()> {
        for ch in text.chars() {
            if ch == '\n' && matches!(self.timing.newline_mode, NewlineMode::ShiftEnter) {
                self.press_modifier(Modifier::Shift)?;
                self.press_key(SpecialKey::Enter)?;
                self.release_modifier(Modifier::Shift)?;
                continue;
            }
            // Skip unsupported characters (non-ASCII) instead of crashing
            if let Err(_) = self.type_char(ch) {
                continue;
            }
            thread::sleep(self.timing.char_delay);
        }
        Ok(())
    }

    fn press_key(&mut self, key: SpecialKey) -> Result<()> {
        let uinput_key = match key {
            SpecialKey::Enter => keyboard::Key::Enter,
            SpecialKey::Backspace => keyboard::Key::BackSpace,
            SpecialKey::Delete => keyboard::Key::Delete,
            SpecialKey::Tab => keyboard::Key::Tab,
            SpecialKey::Space => keyboard::Key::Space,
            SpecialKey::Escape => keyboard::Key::Esc,
            SpecialKey::Left => keyboard::Key::Left,
            SpecialKey::Right => keyboard::Key::Right,
            SpecialKey::Up => keyboard::Key::Up,
            SpecialKey::Down => keyboard::Key::Down,
        };

        self.device.click(&uinput_key)?;
        self.device.synchronize()?;
        // Delay between key events - longer for backspace to ensure apps process them
        // Many apps rate-limit or buffer keyboard input; 15ms seems safe for most
        let delay = if matches!(key, SpecialKey::Backspace) {
            self.timing.backspace_delay
        } else {
            self.timing.key_delay
        };
        thread::sleep(delay);
        Ok(())
    }

    fn delete_words(&mut self, count: usize) -> Result<()> {
        // Use Ctrl+Backspace to delete whole words (much faster than char-by-char)
        for _ in 0..count {
            self.device.press(&keyboard::Key::LeftControl)?;
            self.device.click(&keyboard::Key::BackSpace)?;
            self.device.release(&keyboard::Key::LeftControl)?;
            self.device.synchronize()?;
            thread::sleep(self.timing.delete_word_delay);
        }
        Ok(())
    }

    fn press_modifier(&mut self, modifier: Modifier) -> Result<()> {
        let key = modifier_to_uinput_key(modifier);
        self.device.press(&key)?;
        self.device.synchronize()?;
        Ok(())
    }

    fn release_modifier(&mut self, modifier: Modifier) -> Result<()> {
        let key = modifier_to_uinput_key(modifier);
        self.device.release(&key)?;
        self.device.synchronize()?;
        Ok(())
    }

    fn send_chord(&mut self, modifiers: &[Modifier], ch: char) -> Result<()> {
        // Press all modifiers
        for &m in modifiers {
            self.device.press(&modifier_to_uinput_key(m))?;
        }

        // Press the character key
        let key = char_to_key(ch)?;
        self.device.click(&key)?;

        // Release all modifiers (in reverse order)
        for &m in modifiers.iter().rev() {
            self.device.release(&modifier_to_uinput_key(m))?;
        }

        self.device.synchronize()?;
        thread::sleep(self.timing.chord_delay);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn modifier_to_uinput_key(modifier: Modifier) -> keyboard::Key {
    match modifier {
        Modifier::Ctrl => keyboard::Key::LeftControl,
        Modifier::Shift => keyboard::Key::LeftShift,
        Modifier::Alt => keyboard::Key::LeftAlt,
        Modifier::Super => keyboard::Key::LeftMeta,
    }
}

#[cfg(target_os = "linux")]
fn char_to_key(ch: char) -> Result<keyboard::Key> {
    use keyboard::Key::*;

    let key = match ch.to_ascii_lowercase() {
        'a' => A,
        'b' => B,
        'c' => C,
        'd' => D,
        'e' => E,
        'f' => F,
        'g' => G,
        'h' => H,
        'i' => I,
        'j' => J,
        'k' => K,
        'l' => L,
        'm' => M,
        'n' => N,
        'o' => O,
        'p' => P,
        'q' => Q,
        'r' => R,
        's' => S,
        't' => T,
        'u' => U,
        'v' => V,
        'w' => W,
        'x' => X,
        'y' => Y,
        'z' => Z,

        '0' | ')' => _0,
        '1' | '!' => _1,
        '2' | '@' => _2,
        '3' | '#' => _3,
        '4' | '$' => _4,
        '5' | '%' => _5,
        '6' | '^' => _6,
        '7' | '&' => _7,
        '8' | '*' => _8,
        '9' | '(' => _9,

        ' ' => Space,
        '-' | '_' => Minus,
        '=' | '+' => Equal,
        '[' | '{' => LeftBrace,
        ']' | '}' => RightBrace,
        '\\' | '|' => BackSlash,
        ';' | ':' => SemiColon,
        '\'' | '"' => Apostrophe,
        ',' | '<' => Comma,
        '.' | '>' => Dot,
        '/' | '?' => Slash,
        '`' | '~' => Grave,

        '\n' => Enter,
        '\t' => Tab,

        _ => return Err(anyhow!("Unsupported character: '{}'", ch)),
    };

    Ok(key)
}

// ============================================================================
// Fallback: Enigo Implementation
// ============================================================================

pub struct EnigoKeyboard {
    enigo: Enigo,
    timing: KeyboardTiming,
}

impl EnigoKeyboard {
    pub fn new(timing: KeyboardTiming) -> Result<Self> {
        let enigo = Enigo::new(&Settings::default())
            .context("Failed to initialize enigo keyboard controller")?;
        Ok(Self { enigo, timing })
    }
}

impl VirtualKeyboard for EnigoKeyboard {
    fn type_text(&mut self, text: &str) -> Result<()> {
        for ch in text.chars() {
            if ch == '\n' && matches!(self.timing.newline_mode, NewlineMode::ShiftEnter) {
                self.press_modifier(Modifier::Shift)?;
                self.press_key(SpecialKey::Enter)?;
                self.release_modifier(Modifier::Shift)?;
                continue;
            }
            self.enigo
                .key(enigo::Key::Unicode(ch), Direction::Click)
                .context("Failed to type text with enigo")?;
            thread::sleep(self.timing.char_delay);
        }
        Ok(())
    }

    fn press_key(&mut self, key: SpecialKey) -> Result<()> {
        use enigo::Key::*;

        let enigo_key = match key {
            SpecialKey::Enter => Return,
            SpecialKey::Backspace => Backspace,
            SpecialKey::Delete => Delete,
            SpecialKey::Tab => Tab,
            SpecialKey::Space => Space,
            SpecialKey::Escape => Escape,
            SpecialKey::Left => LeftArrow,
            SpecialKey::Right => RightArrow,
            SpecialKey::Up => UpArrow,
            SpecialKey::Down => DownArrow,
        };

        self.enigo
            .key(enigo_key, Direction::Click)
            .context("Failed to press key with enigo")?;
        let delay = if matches!(key, SpecialKey::Backspace) {
            self.timing.backspace_delay
        } else {
            self.timing.key_delay
        };
        thread::sleep(delay);
        Ok(())
    }

    fn delete_words(&mut self, count: usize) -> Result<()> {
        use enigo::Key::*;
        // Use Ctrl+Backspace to delete whole words
        for _ in 0..count {
            self.enigo
                .key(Control, Direction::Press)
                .context("Failed to press Ctrl")?;
            self.enigo
                .key(Backspace, Direction::Click)
                .context("Failed to press Backspace")?;
            self.enigo
                .key(Control, Direction::Release)
                .context("Failed to release Ctrl")?;
            thread::sleep(self.timing.delete_word_delay);
        }
        Ok(())
    }

    fn press_modifier(&mut self, modifier: Modifier) -> Result<()> {
        let key = modifier_to_enigo_key(modifier);
        self.enigo
            .key(key, Direction::Press)
            .context("Failed to press modifier")
    }

    fn release_modifier(&mut self, modifier: Modifier) -> Result<()> {
        let key = modifier_to_enigo_key(modifier);
        self.enigo
            .key(key, Direction::Release)
            .context("Failed to release modifier")
    }

    fn send_chord(&mut self, modifiers: &[Modifier], ch: char) -> Result<()> {
        use enigo::Key::Unicode;

        // Press all modifiers
        for &m in modifiers {
            self.enigo
                .key(modifier_to_enigo_key(m), Direction::Press)
                .context("Failed to press modifier")?;
        }

        // Press the character key
        self.enigo
            .key(Unicode(ch), Direction::Click)
            .context("Failed to press character")?;

        // Release all modifiers (in reverse order)
        for &m in modifiers.iter().rev() {
            self.enigo
                .key(modifier_to_enigo_key(m), Direction::Release)
                .context("Failed to release modifier")?;
        }

        thread::sleep(self.timing.chord_delay);
        Ok(())
    }
}

fn modifier_to_enigo_key(modifier: Modifier) -> enigo::Key {
    use enigo::Key::*;
    match modifier {
        Modifier::Ctrl => Control,
        Modifier::Shift => Shift,
        Modifier::Alt => Alt,
        Modifier::Super => Meta,
    }
}
