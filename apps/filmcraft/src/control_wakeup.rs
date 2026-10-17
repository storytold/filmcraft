//! Wake eframe's UI pass for control requests even when the compositor withholds a frame callback.

use std::time::Instant;

use eframe::UserEvent;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

fn wake_viewport() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("ai.storyteller.filmcraft.control-wake")
}

fn is_control_wake(event: &UserEvent) -> bool {
    matches!(event, UserEvent::RequestRepaint { viewport_id, .. } if *viewport_id == wake_viewport())
}

#[derive(Clone)]
pub struct ControlWake(EventLoopProxy<UserEvent>);

impl ControlWake {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self(proxy)
    }

    pub fn wake(&self) -> Result<(), String> {
        self.0
            .send_event(UserEvent::RequestRepaint { viewport_id: wake_viewport(), when: Instant::now(), cumulative_pass_nr: 0 })
            .map_err(|e| format!("event loop is unavailable: {e:?}"))
    }
}

/// Eframe's event handler waits for compositor `RedrawRequested` after receiving repaint events.
/// Control wakes are handled directly through a synthetic redraw on the already-created root.
pub struct WakeApplication<'a> {
    inner: eframe::EframeWinitApplication<'a>,
    wake_state: RootWakeState,
}

#[derive(Default)]
struct RootWakeState {
    root: Option<WindowId>,
    pending_control_wake: bool,
}

impl RootWakeState {
    fn control_wake(&mut self) -> Option<WindowId> {
        match self.root {
            Some(root) => Some(root),
            None => {
                self.pending_control_wake = true;
                None
            }
        }
    }

    fn observe_window(&mut self, window_id: WindowId) -> Option<WindowId> {
        if self.root.is_none() {
            self.root = Some(window_id);
        }
        if std::mem::take(&mut self.pending_control_wake) { self.root } else { None }
    }
}

impl<'a> WakeApplication<'a> {
    pub fn new(inner: eframe::EframeWinitApplication<'a>) -> Self {
        Self { inner, wake_state: RootWakeState::default() }
    }
}

impl ApplicationHandler<UserEvent> for WakeApplication<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        self.inner.window_event(event_loop, window_id, event);
        // The first window event identifies eframe's main window. A queued wake may have arrived
        // before this event; replay it after eframe has initialized the window from this event.
        if let Some(root) = self.wake_state.observe_window(window_id) {
            self.inner.window_event(event_loop, root, WindowEvent::RedrawRequested);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let is_wake = is_control_wake(&event);
        self.inner.user_event(event_loop, event);
        if is_wake && let Some(window_id) = self.wake_state.control_wake() {
            self.inner.window_event(event_loop, window_id, WindowEvent::RedrawRequested);
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.inner.new_events(event_loop, cause);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device_id: winit::event::DeviceId, event: DeviceEvent) {
        self.inner.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.memory_warning(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_wake_uses_a_non_root_viewport_marker() {
        assert_ne!(wake_viewport(), egui::ViewportId::ROOT);
        let wake = UserEvent::RequestRepaint { viewport_id: wake_viewport(), when: Instant::now(), cumulative_pass_nr: 0 };
        let repaint = UserEvent::RequestRepaint { viewport_id: egui::ViewportId::ROOT, when: Instant::now(), cumulative_pass_nr: 0 };
        assert!(is_control_wake(&wake));
        assert!(!is_control_wake(&repaint));
    }

    #[test]
    fn control_wake_before_first_window_event_is_replayed_once_root_is_known() {
        let mut state = RootWakeState::default();
        assert_eq!(state.control_wake(), None);

        // Root discovery must not wait for the compositor's first redraw callback.
        let window = WindowId::dummy();
        assert_eq!(state.observe_window(window), Some(window));
        assert_eq!(state.control_wake(), Some(window));
        assert_eq!(state.observe_window(window), None);
    }
}
