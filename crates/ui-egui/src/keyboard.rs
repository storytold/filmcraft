//! Keyboard Input Handling - Fixes for key repeat and double-trigger issues
//!
//! Fixes #3448: Cmd+V spam pastes - triggers on key down AND key up
//! Root cause: Missing key state tracking for modifier keys

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use egui::{Key, Modifiers, Event};

/// Keyboard state tracker to prevent double-trigger and spam
#[derive(Default)]
pub struct KeyboardState {
    /// Currently pressed keys
    pressed_keys: HashMap<Key, KeyState>,
    
    /// Modifier key states
    modifiers: Modifiers,
    
    /// Last key event time for debouncing
    last_event_time: HashMap<Key, Instant>,
    
    /// Minimum time between repeated key events (debounce)
    debounce_ms: u64,
    
    /// Whether we're in a "key down" phase for a shortcut
    shortcut_pending: HashMap<String, bool>,
}

#[derive(Debug, Clone, Copy)]
struct KeyState {
    is_down: bool,
    down_time: Instant,
    repeat_count: u32,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self {
            debounce_ms: 50, // 50ms debounce
            ..Default::default()
        }
    }

    /// Process a raw input event and return filtered events
    pub fn process_event(&mut self, event: &Event) -> Option<Event> {
        match event {
            Event::Key { key, pressed, modifiers, .. } => {
                self.modifiers = *modifiers;
                
                if *pressed {
                    self.handle_key_down(*key, *modifiers)
                } else {
                    self.handle_key_up(*key, *modifiers)
                }
            }
            Event::ModifiersChanged(modifiers) => {
                self.modifiers = *modifiers;
                None
            }
            _ => Some(event.clone()),
        }
    }

    fn handle_key_down(&mut self, key: Key, modifiers: Modifiers) -> Option<Event> {
        let now = Instant::now();
        let key_state = self.pressed_keys.entry(key).or_insert(KeyState {
            is_down: false,
            down_time: now,
            repeat_count: 0,
        });

        // Check debounce
        if let Some(last_time) = self.last_event_time.get(&key) {
            if now.duration_since(*last_time) < Duration::from_millis(self.debounce_ms) {
                // Too soon - this is likely a repeat or duplicate event
                key_state.repeat_count += 1;
                return None;
            }
        }

        // First press or after debounce
        if !key_state.is_down {
            key_state.is_down = true;
            key_state.down_time = now;
            key_state.repeat_count = 0;
            self.last_event_time.insert(key, now);
            
            // Check for shortcut combinations
            self.check_shortcut(key, modifiers, true);
            
            Some(Event::Key { key, pressed: true, modifiers, repeat: false, ..Default::default() })
        } else {
            // Key already down - this is a repeat
            key_state.repeat_count += 1;
            self.last_event_time.insert(key, now);
            
            // Allow repeats for certain keys (navigation, editing)
            if self.should_allow_repeat(key) {
                Some(Event::Key { key, pressed: true, modifiers, repeat: true, ..Default::default() })
            } else {
                None // Suppress repeat for shortcuts like Cmd+V
            }
        }
    }

    fn handle_key_up(&mut self, key: Key, modifiers: Modifiers) -> Option<Event> {
        let now = Instant::now();
        
        if let Some(key_state) = self.pressed_keys.get_mut(&key) {
            key_state.is_down = false;
            
            // Check shortcut on key up (for shortcuts that trigger on release)
            self.check_shortcut(key, modifiers, false);
            
            // Only emit key up if it was actually down
            if key_state.repeat_count == 0 || self.should_emit_key_up(key) {
                self.last_event_time.insert(key, now);
                Some(Event::Key { key, pressed: false, modifiers, repeat: false, ..Default::default() })
            } else {
                None // Suppress key up for held shortcuts
            }
        } else {
            None
        }
    }

    /// Check if this key combination is a registered shortcut
    fn check_shortcut(&mut self, key: Key, modifiers: Modifiers, is_down: bool) {
        let shortcut_id = self.shortcut_id(key, modifiers);
        
        // Shortcuts that should ONLY trigger on key DOWN (not up)
        let down_only_shortcuts = [
            ("v", true),   // Cmd+V / Ctrl+V - Paste
            ("c", true),   // Cmd+C / Ctrl+C - Copy
            ("x", true),   // Cmd+X / Ctrl+X - Cut
            ("z", true),   // Cmd+Z / Ctrl+Z - Undo
            ("y", true),   // Cmd+Y / Ctrl+Y - Redo
            ("a", true),   // Cmd+A / Ctrl+A - Select All
            ("s", true),   // Cmd+S / Ctrl+S - Save
            ("d", true),   // Cmd+D / Ctrl+D - Duplicate
        ];
        
        let is_shortcut = modifiers.command || modifiers.ctrl;
        let key_char = self.key_to_char(key);
        
        if is_shortcut {
            if let Some((_, down_only)) = down_only_shortcuts.iter().find(|(k, _)| *k == key_char) {
                let pending = self.shortcut_pending.entry(shortcut_id.clone()).or_insert(false);
                
                if is_down && !(*pending) {
                    *pending = true;
                    // Allow the key down event through
                } else if !is_down && *pending {
                    *pending = false;
                    // Suppress the key up event for down-only shortcuts
                }
            }
        }
    }

    fn shortcut_id(&self, key: Key, modifiers: Modifiers) -> String {
        format!("{:?}{:?}{:?}{:?}{:?}", 
            modifiers.command, modifiers.ctrl, modifiers.shift, modifiers.alt, key)
    }

    fn key_to_char(&self, key: Key) -> String {
        format!("{:?}", key).to_lowercase()
    }

    /// Keys that should allow repeat events (navigation, etc.)
    fn should_allow_repeat(&self, key: Key) -> bool {
        matches!(key, 
            Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown |
            Key::Home | Key::End | Key::PageUp | Key::PageDown |
            Key::Backspace | Key::Delete |
            Key::Tab
        )
    }

    /// Keys that should emit key up events even after repeats
    fn should_emit_key_up(&self, key: Key) -> bool {
        !matches!(key, 
            Key::V | Key::C | Key::X | Key::Z | Key::Y | Key::A | Key::S | Key::D
        )
    }

    /// Get current modifier state
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Check if a key is currently pressed
    pub fn is_key_down(&self, key: Key) -> bool {
        self.pressed_keys.get(&key).map(|s| s.is_down).unwrap_or(false)
    }

    /// Check if a shortcut combination is currently held
    pub fn is_shortcut_held(&self, key: Key, modifiers: Modifiers) -> bool {
        let shortcut_id = self.shortcut_id(key, modifiers);
        self.shortcut_pending.get(&shortcut_id).copied().unwrap_or(false)
    }

    /// Clear all state (e.g., on focus loss)
    pub fn clear(&mut self) {
        self.pressed_keys.clear();
        self.shortcut_pending.clear();
        self.modifiers = Modifiers::default();
    }

    /// Set debounce time
    pub fn set_debounce(&mut self, ms: u64) {
        self.debounce_ms = ms;
    }
}

/// Global keyboard state for the application
pub struct GlobalKeyboardState {
    state: Arc<Mutex<KeyboardState>>,
}

impl GlobalKeyboardState {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(KeyboardState::new())),
        }
    }

    pub fn process_event(&self, event: &Event) -> Option<Event> {
        self.state.lock().unwrap().process_event(event)
    }

    pub fn modifiers(&self) -> Modifiers {
        self.state.lock().unwrap().modifiers()
    }

    pub fn is_key_down(&self, key: Key) -> bool {
        self.state.lock().unwrap().is_key_down(key)
    }

    pub fn clear(&self) {
        self.state.lock().unwrap().clear();
    }
}

/// Integration with egui input handling
pub fn filter_input_events(events: &mut Vec<Event>, keyboard_state: &GlobalKeyboardState) {
    let mut filtered = Vec::new();
    
    for event in events.drain(..) {
        if let Some(filtered_event) = keyboard_state.process_event(&event) {
            filtered.push(filtered_event);
        }
        // Dropped events are filtered out
    }
    
    *events = filtered;
}

/// Key binding definitions for the application
pub mod shortcuts {
    use super::*;
    use egui::{Key, Modifiers};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Action {
        // Edit
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        PasteInsert,
        SelectAll,
        DeselectAll,
        Duplicate,
        
        // Timeline
        RippleDelete,
        RippleDeleteHead,
        AddEdit,
        Razor,
        
        // Playback
        PlayPause,
        Stop,
        StepForward,
        StepBackward,
        
        // Tools
        SelectionTool,
        RippleTool,
        RollTool,
        SlipTool,
        SlideTool,
        RateStretchTool,
        RazorTool,
        HandTool,
        ZoomTool,
        
        // View
        ZoomIn,
        ZoomOut,
        FitToWindow,
        
        // File
        Save,
        SaveAs,
        Import,
        Export,
    }

    pub fn default_shortcut(action: Action) -> (Key, Modifiers) {
        use Modifiers as M;
        match action {
            Action::Undo => (Key::Z, M::COMMAND),
            Action::Redo => (Key::Z, M::COMMAND | M::SHIFT),
            Action::Cut => (Key::X, M::COMMAND),
            Action::Copy => (Key::C, M::COMMAND),
            Action::Paste => (Key::V, M::COMMAND),
            Action::PasteInsert => (Key::V, M::COMMAND | M::SHIFT),
            Action::SelectAll => (Key::A, M::COMMAND),
            Action::DeselectAll => (Key::A, M::COMMAND | M::SHIFT),
            Action::Duplicate => (Key::D, M::COMMAND | M::SHIFT),
            
            Action::RippleDelete => (Key::Delete, M::SHIFT),
            Action::RippleDeleteHead => (Key::Delete, M::COMMAND | M::SHIFT),
            Action::AddEdit => (Key::K, M::COMMAND),
            Action::Razor => (Key::C, M::empty()),
            
            Action::PlayPause => (Key::Space, M::empty()),
            Action::Stop => (Key::K, M::empty()),
            Action::StepForward => (Key::ArrowRight, M::empty()),
            Action::StepBackward => (Key::ArrowLeft, M::empty()),
            
            Action::SelectionTool => (Key::V, M::empty()),
            Action::RippleTool => (Key::B, M::empty()),
            Action::RollTool => (Key::N, M::empty()),
            Action::SlipTool => (Key::Y, M::empty()),
            Action::SlideTool => (Key::U, M::empty()),
            Action::RateStretchTool => (Key::R, M::empty()),
            Action::RazorTool => (Key::C, M::empty()),
            Action::HandTool => (Key::H, M::empty()),
            Action::ZoomTool => (Key::Z, M::empty()),
            
            Action::ZoomIn => (Key::Equal, M::COMMAND),
            Action::ZoomOut => (Key::Minus, M::COMMAND),
            Action::FitToWindow => (Key::Backslash, M::COMMAND),
            
            Action::Save => (Key::S, M::COMMAND),
            Action::SaveAs => (Key::S, M::COMMAND | M::SHIFT),
            Action::Import => (Key::I, M::COMMAND),
            Action::Export => (Key::E, M::COMMAND | M::SHIFT),
        }
    }

    pub fn matches(event: &Event, action: Action) -> bool {
        if let Event::Key { key, pressed: true, modifiers, .. } = event {
            let (expected_key, expected_mods) = default_shortcut(action);
            *key == expected_key && *modifiers == expected_mods
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers, Event};

    #[test]
    fn test_keyboard_state_basic() {
        let mut state = KeyboardState::new();
        
        let event = Event::Key { key: Key::V, pressed: true, modifiers: Modifiers::COMMAND, repeat: false, ..Default::default() };
        let result = state.process_event(&event);
        assert!(result.is_some());
        
        // Second key down should be suppressed (debounce)
        let result2 = state.process_event(&event);
        assert!(result2.is_none());
    }

    #[test]
    fn test_cmd_v_not_spam() {
        let mut state = KeyboardState::new();
        
        // Key down
        let down = Event::Key { key: Key::V, pressed: true, modifiers: Modifiers::COMMAND, repeat: false, ..Default::default() };
        let result = state.process_event(&down);
        assert!(result.is_some());
        
        // Key up - should be suppressed for Cmd+V
        let up = Event::Key { key: Key::V, pressed: false, modifiers: Modifiers::COMMAND, repeat: false, ..Default::default() };
        let result = state.process_event(&up);
        assert!(result.is_none());
    }

    #[test]
    fn test_navigation_keys_allow_repeat() {
        let mut state = KeyboardState::new();
        
        let event = Event::Key { key: Key::ArrowRight, pressed: true, modifiers: Modifiers::empty(), repeat: false, ..Default::default() };
        let result = state.process_event(&event);
        assert!(result.is_some());
        
        // Repeat should be allowed for navigation
        let repeat = Event::Key { key: Key::ArrowRight, pressed: true, modifiers: Modifiers::empty(), repeat: true, ..Default::default() };
        let result = state.process_event(&repeat);
        assert!(result.is_some());
    }

    #[test]
    fn test_shortcut_matching() {
        let event = Event::Key { key: Key::V, pressed: true, modifiers: Modifiers::COMMAND, repeat: false, ..Default::default() };
        assert!(shortcuts::matches(&event, shortcuts::Action::Paste));
        assert!(!shortcuts::matches(&event, shortcuts::Action::Copy));
    }
}