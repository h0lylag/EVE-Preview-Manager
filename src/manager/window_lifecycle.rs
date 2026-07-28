//! Manager window visibility lifecycle and tray restore signaling.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use eframe::egui;

/// The Manager window state owned by the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowState {
    Shown,
    HiddenToTray,
    RestoringFromTray,
}

/// How the Manager window should behave during startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupMode {
    Show,
    HideWhenTrayReady,
}

/// A visibility transition requested for the Manager window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowAction {
    HideToTray,
    ShowAndFocus,
}

/// Coalesces show requests sent from the tray thread.
#[derive(Clone, Debug, Default)]
pub(crate) struct ShowWindowSignal {
    requested: Arc<AtomicBool>,
}

impl ShowWindowSignal {
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Relaxed);
    }

    fn take(&self) -> bool {
        self.requested.swap(false, Ordering::Relaxed)
    }
}

/// Current inputs used to reconcile the Manager window lifecycle.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowConditions {
    pub(crate) minimize_to_tray_enabled: bool,
    pub(crate) start_hidden_enabled: bool,
    pub(crate) tray_ready: bool,
    pub(crate) is_minimized: bool,
}

/// Owns Manager window state and emits the required eframe viewport commands.
#[derive(Debug)]
pub(crate) struct WindowLifecycle {
    state: WindowState,
    startup_mode: StartupMode,
    show_signal: ShowWindowSignal,
}

impl WindowLifecycle {
    pub(crate) fn new(startup_mode: StartupMode) -> Self {
        Self {
            state: WindowState::Shown,
            startup_mode,
            show_signal: ShowWindowSignal::default(),
        }
    }

    pub(crate) fn show_signal(&self) -> ShowWindowSignal {
        self.show_signal.clone()
    }

    /// Reconcile external requests, configuration, and native window state.
    pub(crate) fn update(&mut self, ctx: &egui::Context, conditions: WindowConditions) {
        if self.show_signal.take() {
            self.startup_mode = StartupMode::Show;
            self.apply(ctx, WindowAction::ShowAndFocus);
            return;
        }

        // Restoring a minimized X11 window is asynchronous. Ignore the stale minimized
        // state until eframe observes the native window as restored, or we could hide it
        // again immediately after a tray click.
        if self.state == WindowState::RestoringFromTray {
            if !conditions.is_minimized {
                self.state = WindowState::Shown;
            }
            return;
        }

        if self.startup_mode == StartupMode::HideWhenTrayReady
            && (!conditions.minimize_to_tray_enabled || !conditions.start_hidden_enabled)
        {
            self.startup_mode = StartupMode::Show;
        }

        if !conditions.tray_ready {
            return;
        }

        if self.startup_mode == StartupMode::HideWhenTrayReady {
            self.startup_mode = StartupMode::Show;
            self.apply(ctx, WindowAction::HideToTray);
            return;
        }

        if conditions.minimize_to_tray_enabled
            && conditions.is_minimized
            && self.state == WindowState::Shown
        {
            self.apply(ctx, WindowAction::HideToTray);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: WindowAction) {
        match action {
            WindowAction::HideToTray => {
                if self.state == WindowState::HiddenToTray {
                    return;
                }

                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.state = WindowState::HiddenToTray;
            }
            WindowAction::ShowAndFocus => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.state = WindowState::RestoringFromTray;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_update(
        lifecycle: &mut WindowLifecycle,
        conditions: WindowConditions,
    ) -> egui::FullOutput {
        egui::Context::default().run_ui(egui::RawInput::default(), |ui| {
            lifecycle.update(ui.ctx(), conditions);
        })
    }

    fn root_commands(output: &egui::FullOutput) -> &[egui::ViewportCommand] {
        &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output should always exist")
            .commands
    }

    fn ready_conditions() -> WindowConditions {
        WindowConditions {
            minimize_to_tray_enabled: true,
            start_hidden_enabled: false,
            tray_ready: true,
            is_minimized: true,
        }
    }

    #[test]
    fn hide_queues_ordered_commands_and_updates_state() {
        let mut lifecycle = WindowLifecycle::new(StartupMode::Show);

        let output = run_update(&mut lifecycle, ready_conditions());

        assert_eq!(
            root_commands(&output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(false),
            ]
        );
        assert_eq!(lifecycle.state, WindowState::HiddenToTray);
    }

    #[test]
    fn duplicate_hide_is_a_noop() {
        let mut lifecycle = WindowLifecycle::new(StartupMode::Show);
        let conditions = ready_conditions();
        let _ = run_update(&mut lifecycle, conditions);

        let output = run_update(&mut lifecycle, conditions);

        assert!(root_commands(&output).is_empty());
        assert_eq!(lifecycle.state, WindowState::HiddenToTray);
    }

    #[test]
    fn show_queues_restore_commands_and_updates_state() {
        let mut lifecycle = WindowLifecycle::new(StartupMode::Show);
        let _ = run_update(&mut lifecycle, ready_conditions());
        lifecycle.show_signal.request();

        let output = run_update(&mut lifecycle, ready_conditions());

        assert_eq!(
            root_commands(&output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(true),
                egui::ViewportCommand::Focus,
            ]
        );
        assert_eq!(lifecycle.state, WindowState::RestoringFromTray);
    }

    #[test]
    fn show_while_shown_still_requests_focus() {
        let mut lifecycle = WindowLifecycle::new(StartupMode::Show);
        lifecycle.show_signal.request();

        let output = run_update(
            &mut lifecycle,
            WindowConditions {
                is_minimized: false,
                ..ready_conditions()
            },
        );

        assert!(root_commands(&output).contains(&egui::ViewportCommand::Focus));
        assert_eq!(lifecycle.state, WindowState::RestoringFromTray);
    }

    #[test]
    fn restore_waits_for_native_unminimize_before_accepting_another_hide() {
        let mut lifecycle = WindowLifecycle::new(StartupMode::Show);
        let _ = run_update(&mut lifecycle, ready_conditions());
        lifecycle.show_signal.request();

        let show_output = run_update(&mut lifecycle, ready_conditions());
        assert_eq!(
            root_commands(&show_output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(true),
                egui::ViewportCommand::Focus,
            ]
        );
        assert_eq!(lifecycle.state, WindowState::RestoringFromTray);

        let stale_minimized_output = run_update(&mut lifecycle, ready_conditions());
        assert!(root_commands(&stale_minimized_output).is_empty());
        assert_eq!(lifecycle.state, WindowState::RestoringFromTray);

        let restored_output = run_update(
            &mut lifecycle,
            WindowConditions {
                is_minimized: false,
                ..ready_conditions()
            },
        );
        assert!(root_commands(&restored_output).is_empty());
        assert_eq!(lifecycle.state, WindowState::Shown);

        let future_hide_output = run_update(&mut lifecycle, ready_conditions());
        assert_eq!(
            root_commands(&future_hide_output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(false),
            ]
        );
        assert_eq!(lifecycle.state, WindowState::HiddenToTray);
    }

    #[test]
    fn repeated_show_requests_are_coalesced() {
        let signal = ShowWindowSignal::default();
        signal.request();
        signal.request();

        assert!(signal.take());
        assert!(!signal.take());
    }
}
