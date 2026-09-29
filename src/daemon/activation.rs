//! Session-only activation transactions and observed-focus visibility policy.

use std::time::{Duration, Instant};

use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, Window};

use super::border_update::{BorderFocus, sync_focused_borders};
use super::dispatcher::EventContext;
use super::focus::{self, FocusObservation, FocusOwner};
use super::handlers;
use super::session_state::SessionState;
use crate::common::types::SourceIdentity;
use crate::x11::{activate_window, minimize_window, refresh_pointer_state, unminimize_window};

const PROBE_INTERVAL: Duration = Duration::from_millis(50);
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(1000);
const HIDE_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivationOrigin {
    Hotkey,
    Click,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationOutcome {
    Confirmed,
    Cancelled(&'static str),
}

#[derive(Debug)]
struct PendingActivation {
    generation: u64,
    target: Window,
    origin: Option<FocusObservation>,
    requested_at: Instant,
    deadline: Instant,
    minimize: bool,
    kind: ActivationOrigin,
}

#[derive(Default)]
pub(super) struct FocusRuntime {
    generation: u64,
    pending: Option<PendingActivation>,
    last_observation: Option<FocusObservation>,
    next_probe: Option<Instant>,
    // This belongs to the continuous focus transition, not to a request generation.
    unresolved_since: Option<Instant>,
    observation_failed: bool,
}

impl Drop for FocusRuntime {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            debug!(
                generation = pending.generation,
                target = pending.target,
                "Activation cancelled: session ended"
            );
        }
    }
}

impl FocusRuntime {
    fn request(
        &mut self,
        target: Window,
        origin: Option<FocusObservation>,
        minimize: bool,
        kind: ActivationOrigin,
        now: Instant,
    ) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(previous) = self.pending.take() {
            debug!(
                generation = previous.generation,
                target = previous.target,
                "Activation superseded"
            );
        }
        self.pending = Some(PendingActivation {
            generation: self.generation,
            target,
            origin,
            requested_at: now,
            deadline: now + ACTIVATION_TIMEOUT,
            minimize,
            kind,
        });
        self.next_probe = Some(now);
    }

    fn outcome(
        &self,
        observation: Option<FocusObservation>,
        target_exists: bool,
        now: Instant,
    ) -> Option<ActivationOutcome> {
        let pending = self.pending.as_ref()?;
        if !target_exists {
            return Some(ActivationOutcome::Cancelled("target removed"));
        }
        if now >= pending.deadline {
            return Some(ActivationOutcome::Cancelled("timeout"));
        }
        let observation = observation?;
        if observation.explicit_source() == Some(pending.target) {
            return Some(ActivationOutcome::Confirmed);
        }
        if !observation.pointer_root
            && matches!(observation.owner, FocusOwner::Outside(_))
            && pending
                .origin
                .is_some_and(|origin| origin.pointer_root || origin.owner != observation.owner)
        {
            return Some(ActivationOutcome::Cancelled("outside focus departure"));
        }
        None
    }
}

/// Effective hide time preserves the original hysteresis across requests and grace expiry.
fn hide_deadline(session: &SessionState) -> Option<Instant> {
    if session.focus.observation_failed {
        return None;
    }
    let original = session.focus_loss_deadline?;
    match (&session.focus.pending, session.focus.unresolved_since) {
        (Some(pending), Some(since)) => {
            Some(original.max(pending.deadline.min(since + ACTIVATION_TIMEOUT)))
        }
        _ => Some(original),
    }
}

pub(super) fn next_deadline(session: &SessionState) -> Option<Instant> {
    [
        session.focus.next_probe,
        session
            .focus
            .pending
            .as_ref()
            .map(|pending| pending.deadline),
        hide_deadline(session),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// Record the requested cursor independently of whatever currently owns X focus.
pub(super) fn begin(
    ctx: &mut EventContext<'_, '_>,
    target: Window,
    identity: Option<&SourceIdentity>,
    timestamp: u32,
    kind: ActivationOrigin,
) {
    let origin = focus::observe(
        ctx.app_ctx,
        ctx.eve_clients,
        ctx.cycle_state.get_active_windows(),
    )
    .ok();
    let now = Instant::now();
    let minimize = ctx.daemon_config.profile.client_minimize_on_switch;
    ctx.session_state
        .focus
        .request(target, origin, minimize, kind, now);
    ctx.cycle_state
        .set_current_by_window_with_identity(target, identity);
    sync_focused_borders(
        ctx.eve_clients,
        ctx.cycle_state,
        ctx.display_config,
        ctx.font_renderer,
        target,
        BorderFocus::Requested,
        "activation requested",
    );

    let map_state = ctx
        .app_ctx
        .conn
        .get_window_attributes(target)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|attrs| attrs.map_state);
    debug!(
        generation = ctx.session_state.focus.generation,
        target,
        ?origin,
        ?map_state,
        ?kind,
        minimize,
        timestamp,
        "Requesting activation"
    );
    let result = (|| {
        if minimize {
            unminimize_window(ctx.app_ctx.conn, target)?;
        }
        activate_window(
            ctx.app_ctx.conn,
            ctx.app_ctx.screen,
            ctx.app_ctx.atoms,
            target,
            timestamp,
        )
    })();
    if let Err(error) = result {
        warn!(generation = ctx.session_state.focus.generation, target, %error, "Activation cancelled: request failed");
        ctx.session_state.focus.pending = None;
    } else if kind == ActivationOrigin::Hotkey
        && let Err(error) = refresh_pointer_state(ctx.app_ctx.conn, target, timestamp)
    {
        debug!(target, %error, "Failed to refresh pointer after activation request");
    }
    let _ = ctx.app_ctx.conn.flush();
    reconcile(ctx, Instant::now());
}

/// Minimize only while the confirmed target still explicitly owns focus.
fn minimize_after_confirmation(ctx: &mut EventContext<'_, '_>, pending: &PendingActivation) {
    if !pending.minimize {
        return;
    }
    let mut windows = handlers::source_windows_to_minimize(
        ctx.cycle_state,
        ctx.session_state,
        ctx.display_config,
        pending.target,
    );
    if pending.kind == ActivationOrigin::Hotkey
        && let Some(manager) =
            crate::x11::get_client_list(ctx.app_ctx.conn, ctx.app_ctx.screen, ctx.app_ctx.atoms)
                .ok()
                .and_then(|windows| {
                    windows.into_iter().find(|&window| {
                        crate::x11::get_window_class(ctx.app_ctx.conn, window, ctx.app_ctx.atoms)
                            .ok()
                            .flatten()
                            .is_some_and(|class| class == "eve-preview-manager")
                    })
                })
        && manager != pending.target
        && !windows.contains(&manager)
    {
        windows.push(manager);
    }
    for window in windows {
        let observed = focus::observe(
            ctx.app_ctx,
            ctx.eve_clients,
            ctx.cycle_state.get_active_windows(),
        );
        if observed
            .as_ref()
            .ok()
            .and_then(|focus| focus.explicit_source())
            != Some(pending.target)
        {
            debug!(
                generation = pending.generation,
                target = pending.target,
                ?observed,
                "Stopped minimization: target no longer confirmed"
            );
            break;
        }
        debug!(
            generation = pending.generation,
            target = pending.target,
            window,
            ?observed,
            "Minimizing after confirmed activation"
        );
        if let Err(error) = minimize_window(
            ctx.app_ctx.conn,
            ctx.app_ctx.screen,
            ctx.app_ctx.atoms,
            window,
        ) {
            warn!(window, %error, "Failed to minimize after activation");
        }
    }
}

/// Query current focus; event mode/detail and optimistic borders are never evidence.
pub(super) fn reconcile(ctx: &mut EventContext<'_, '_>, now: Instant) {
    let observed = focus::observe(
        ctx.app_ctx,
        ctx.eve_clients,
        ctx.cycle_state.get_active_windows(),
    );
    // Decide at the time the synchronous focus query completed: a delayed reply must not
    // count as arriving before a deadline. Explicit (future) test clocks stay authoritative.
    let now = now.max(Instant::now());
    let observation = observed.as_ref().ok().copied();
    let target_exists = ctx
        .session_state
        .focus
        .pending
        .as_ref()
        .is_none_or(|pending| {
            ctx.cycle_state
                .get_active_windows()
                .contains_key(&pending.target)
                || ctx.eve_clients.contains_key(&pending.target)
        });
    if let Some(reason) = ctx
        .session_state
        .focus
        .outcome(observation, target_exists, now)
        && let Some(pending) = ctx.session_state.focus.pending.take()
    {
        debug!(
            generation = pending.generation,
            target = pending.target,
            ?reason,
            elapsed_ms = now.duration_since(pending.requested_at).as_millis(),
            ?observation,
            "Activation completed"
        );
        if reason == ActivationOutcome::Confirmed {
            minimize_after_confirmation(ctx, &pending);
        }
    }

    let Ok(observation) = observed else {
        if !ctx.session_state.focus.observation_failed {
            debug!(error = ?observed.err(), "Focus unresolved; preserving visibility and retrying");
        }
        ctx.session_state.focus.observation_failed = true;
        ctx.session_state.focus.next_probe = Some(now + PROBE_INTERVAL);
        return;
    };
    let runtime = &mut ctx.session_state.focus;
    if runtime.last_observation != Some(observation) || runtime.observation_failed {
        debug!(raw_focus = observation.raw_focus, owner = ?observation.owner, pointer_root = observation.pointer_root, "Observed focus ownership");
    }
    runtime.observation_failed = false;
    runtime.last_observation = Some(observation);
    runtime.next_probe =
        (runtime.pending.is_some() || observation.needs_poll()).then_some(now + PROBE_INTERVAL);

    // The action schedules any focus-loss deadline that the active-preview policy reads.
    let action = visibility_action(
        ctx.session_state,
        ctx.display_config.hide_when_no_focus,
        observation,
        now,
    );
    // Settle the active-preview block before any handler below reconciles previews.
    if ctx.display_config.hide_active {
        let session = &mut *ctx.session_state;
        let retain_during_grace = ctx.display_config.hide_when_no_focus
            && session.focus_loss_deadline.is_some()
            && !session.focus_hidden;
        session
            .preview_visibility
            .observe(observation.owner, retain_during_grace);
    }
    match action {
        VisibilityAction::Restore => handlers::state::restore_focus_visibility(ctx),
        VisibilityAction::Hide => {
            if let Err(error) = handlers::input::cancel_group_drag(
                ctx.app_ctx.conn,
                ctx.eve_clients,
                ctx.group_drag_state,
                None,
            ) {
                warn!(%error, "Failed to restore group drag before focus-loss hide");
            }
            handlers::state::hide_after_focus_loss(ctx);
        }
        VisibilityAction::Keep if ctx.display_config.hide_active => {
            handlers::state::reconcile_previews(ctx);
        }
        VisibilityAction::Keep => {}
    }

    let requested = ctx
        .session_state
        .focus
        .pending
        .as_ref()
        .map(|pending| pending.target);
    // Requests and indirect (frame) ownership never promote minimized rendering; only
    // explicit X focus proves the source is viewable.
    let focused = if let Some(target) = requested {
        Some((target, BorderFocus::Requested))
    } else if !observation.pointer_root {
        match observation.owner {
            FocusOwner::Source(source) | FocusOwner::Frame(source) => {
                let remembered = ctx
                    .session_state
                    .window_last_character
                    .get(&source)
                    .cloned()
                    .map(SourceIdentity::eve);
                ctx.cycle_state
                    .set_current_by_window_with_identity(source, remembered.as_ref());
                let kind = if matches!(observation.owner, FocusOwner::Source(_)) {
                    BorderFocus::Observed
                } else {
                    BorderFocus::Requested
                };
                Some((source, kind))
            }
            FocusOwner::Outside(_) | FocusOwner::Root | FocusOwner::None => {
                if let Some(previous) = ctx.cycle_state.get_current_window() {
                    debug!(previous, "Cleared source focus cursor after outside focus");
                    ctx.cycle_state.clear_current_window();
                }
                Some((0, BorderFocus::Requested))
            }
            FocusOwner::Preview(_) => None,
        }
    } else {
        None
    };
    if let Some((window, kind)) = focused {
        sync_focused_borders(
            ctx.eve_clients,
            ctx.cycle_state,
            ctx.display_config,
            ctx.font_renderer,
            window,
            kind,
            "observed focus reconciliation",
        );
    }
}

/// Whether unmapping or destroying `window` can change focus ownership.
///
/// Tracked sources can end a transaction or lose focus. Any window holding the last observed
/// X focus can revert it; reverting to root/None/PointerRoot also reaches the root window as a
/// focus event, and frame/preview ownership keeps its periodic recheck. Other windows,
/// including our own previews unmapped by focus-loss hiding, cannot.
pub(super) fn structure_change_affects_focus(ctx: &EventContext<'_, '_>, window: Window) -> bool {
    ctx.eve_clients.contains_key(&window)
        || ctx.cycle_state.get_active_windows().contains_key(&window)
        || ctx
            .session_state
            .focus
            .last_observation
            .is_some_and(|observation| observation.raw_focus == window)
}

#[derive(Debug, PartialEq, Eq)]
enum VisibilityAction {
    Keep,
    Restore,
    Hide,
}

fn visibility_action(
    session: &mut SessionState,
    hide_enabled: bool,
    observation: FocusObservation,
    now: Instant,
) -> VisibilityAction {
    if observation.owner.source().is_some() {
        session.focus.unresolved_since = None;
        return VisibilityAction::Restore;
    }
    if observation.transient() {
        // Retain this anchor across superseding requests and timeouts.
        session.focus.unresolved_since.get_or_insert(now);
    } else {
        session.focus.unresolved_since = None;
    }
    if hide_enabled && !session.focus_hidden {
        if session.focus_loss_deadline.is_none() {
            session.focus_loss_deadline = Some(now + HIDE_DELAY);
            debug!(deadline = ?session.focus_loss_deadline, grace_since = ?session.focus.unresolved_since, "Scheduled focus-loss hide");
        }
        if hide_deadline(session).is_some_and(|deadline| now >= deadline) {
            return VisibilityAction::Hide;
        }
    }
    VisibilityAction::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(owner: FocusOwner) -> FocusObservation {
        FocusObservation {
            raw_focus: 100,
            owner,
            pointer_root: false,
        }
    }

    fn request(state: &mut FocusRuntime, target: Window, origin: FocusOwner, now: Instant) {
        state.request(
            target,
            Some(observation(origin)),
            true,
            ActivationOrigin::Hotkey,
            now,
        );
    }

    #[test]
    fn only_latest_explicit_target_can_confirm_once() {
        let start = Instant::now();
        let mut state = FocusRuntime::default();
        request(&mut state, 20, FocusOwner::Source(10), start);
        for owner in [
            FocusOwner::Source(10),
            FocusOwner::Frame(20),
            FocusOwner::Preview(20),
            FocusOwner::Root,
            FocusOwner::None,
        ] {
            assert_eq!(state.outcome(Some(observation(owner)), true, start), None);
        }
        let pointer = FocusObservation {
            pointer_root: true,
            ..observation(FocusOwner::Source(20))
        };
        assert_eq!(state.outcome(Some(pointer), true, start), None);
        assert_eq!(
            state.outcome(None, true, start),
            None,
            "query failure cannot confirm"
        );
        let generation = state.pending.as_ref().unwrap().generation;
        request(
            &mut state,
            30,
            FocusOwner::Source(10),
            start + PROBE_INTERVAL,
        );
        assert!(state.pending.as_ref().unwrap().generation > generation);
        assert_eq!(
            state.outcome(
                Some(observation(FocusOwner::Source(20))),
                true,
                start + PROBE_INTERVAL
            ),
            None,
            "late old focus cannot confirm latest request"
        );
        assert_eq!(state.pending.as_ref().unwrap().target, 30);
        assert_eq!(
            state.outcome(
                Some(observation(FocusOwner::Source(30))),
                true,
                start + PROBE_INTERVAL
            ),
            Some(ActivationOutcome::Confirmed)
        );
        state.pending.take();
        assert_eq!(
            state.outcome(
                Some(observation(FocusOwner::Source(30))),
                true,
                start + PROBE_INTERVAL
            ),
            None
        );
    }

    #[test]
    fn already_focused_and_delayed_activation_confirm_before_deadline_only() {
        let start = Instant::now();
        for delay in [0, 50, 999, 1000, 1200] {
            let mut state = FocusRuntime::default();
            request(&mut state, 20, FocusOwner::Source(20), start);
            assert_eq!(
                state.outcome(
                    Some(observation(FocusOwner::Source(20))),
                    true,
                    start + Duration::from_millis(delay)
                ),
                Some(if delay < 1000 {
                    ActivationOutcome::Confirmed
                } else {
                    ActivationOutcome::Cancelled("timeout")
                })
            );
        }
    }

    #[test]
    fn failed_observations_refusal_and_destroyed_targets_never_confirm() {
        let start = Instant::now();
        let mut state = FocusRuntime::default();
        request(&mut state, 20, FocusOwner::Source(10), start);
        assert_eq!(
            state.outcome(None, true, start + ACTIVATION_TIMEOUT),
            Some(ActivationOutcome::Cancelled("timeout"))
        );
        assert_eq!(
            state.outcome(
                Some(observation(FocusOwner::Source(10))),
                true,
                start + ACTIVATION_TIMEOUT
            ),
            Some(ActivationOutcome::Cancelled("timeout"))
        );
        assert_eq!(
            state.outcome(Some(observation(FocusOwner::Source(20))), false, start),
            Some(ActivationOutcome::Cancelled("target removed"))
        );
    }

    #[test]
    fn original_outside_owner_is_allowed_but_departure_cancels() {
        let start = Instant::now();
        let mut state = FocusRuntime::default();
        request(&mut state, 20, FocusOwner::Outside(40), start);
        assert_eq!(
            state.outcome(Some(observation(FocusOwner::Outside(40))), true, start),
            None
        );
        assert_eq!(
            state.outcome(Some(observation(FocusOwner::Outside(50))), true, start),
            Some(ActivationOutcome::Cancelled("outside focus departure"))
        );
        let pointer = FocusObservation {
            pointer_root: true,
            ..observation(FocusOwner::Outside(50))
        };
        assert_eq!(state.outcome(Some(pointer), true, start), None);
        request(&mut state, 20, FocusOwner::Source(10), start);
        assert_eq!(
            state.outcome(Some(observation(FocusOwner::Outside(40))), true, start),
            Some(ActivationOutcome::Cancelled("outside focus departure"))
        );
    }

    #[test]
    fn outside_hysteresis_does_not_restart_and_eligible_focus_restores() {
        let start = Instant::now();
        let mut session = SessionState::default();
        let outside = observation(FocusOwner::Outside(40));
        for delay in [0, 30, 70, 99] {
            assert_eq!(
                visibility_action(
                    &mut session,
                    true,
                    outside,
                    start + Duration::from_millis(delay)
                ),
                VisibilityAction::Keep
            );
            assert_eq!(session.focus_loss_deadline, Some(start + HIDE_DELAY));
        }
        assert_eq!(
            visibility_action(&mut session, true, outside, start + HIDE_DELAY),
            VisibilityAction::Hide
        );
        session.focus_hidden = true;
        session.focus_loss_deadline = None;
        for owner in [
            FocusOwner::Source(20),
            FocusOwner::Frame(20),
            FocusOwner::Preview(20),
        ] {
            assert_eq!(
                visibility_action(&mut session, true, observation(owner), start + HIDE_DELAY),
                VisibilityAction::Restore
            );
        }
        assert_eq!(
            visibility_action(
                &mut SessionState::default(),
                false,
                outside,
                start + HIDE_DELAY
            ),
            VisibilityAction::Keep
        );
    }

    #[test]
    fn repeated_requests_cannot_extend_continuous_transition_grace() {
        let start = Instant::now();
        let mut session = SessionState::default();
        request(&mut session.focus, 20, FocusOwner::Source(10), start);
        let unresolved = observation(FocusOwner::Root);
        assert_eq!(
            visibility_action(&mut session, true, unresolved, start),
            VisibilityAction::Keep
        );
        for delay in [50, 250, 500, 999] {
            let now = start + Duration::from_millis(delay);
            request(&mut session.focus, 30, FocusOwner::Root, now);
            assert_eq!(
                visibility_action(&mut session, true, unresolved, now),
                VisibilityAction::Keep
            );
            assert_eq!(hide_deadline(&session), Some(start + ACTIVATION_TIMEOUT));
        }
        assert_eq!(
            visibility_action(&mut session, true, unresolved, start + ACTIVATION_TIMEOUT),
            VisibilityAction::Hide
        );
        assert_eq!(session.focus_loss_deadline, Some(start + HIDE_DELAY));
        // Cancelling the transaction resumes the existing deadline; no second hysteresis.
        session.focus.pending = None;
        assert_eq!(hide_deadline(&session), Some(start + HIDE_DELAY));
    }

    #[test]
    fn unknown_query_defers_hide_without_proving_activation_or_resetting_deadline() {
        let start = Instant::now();
        let mut session = SessionState::default();
        visibility_action(&mut session, true, observation(FocusOwner::Root), start);
        session.focus.observation_failed = true;
        session.focus.next_probe = Some(start + PROBE_INTERVAL);
        assert_eq!(hide_deadline(&session), None);
        assert_eq!(next_deadline(&session), Some(start + PROBE_INTERVAL));
        session.focus.observation_failed = false;
        assert_eq!(
            visibility_action(
                &mut session,
                true,
                observation(FocusOwner::Root),
                start + HIDE_DELAY
            ),
            VisibilityAction::Hide
        );
        assert_eq!(session.focus_loss_deadline, Some(start + HIDE_DELAY));
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use crate::common::types::{Dimensions, PreviewMode, SourceKind};
    use crate::config::{
        DaemonConfig,
        profile::{CycleSlot, Profile},
    };
    use crate::daemon::{
        cycle_state::CycleState, dispatcher::handle_event, font::FontRenderer,
        group_drag::GroupDragState, thumbnail::Thumbnail,
    };
    use crate::x11::{AppContext, CachedAtoms, CachedFormats};
    use std::collections::HashMap;
    use x11rb::protocol::{Event, xproto::*};
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    #[derive(Clone, Copy)]
    struct Windows {
        a: Window,
        b: Window,
        manager: Window,
    }

    fn window(ctx: &AppContext<'_>, parent: Window) -> Window {
        let id = ctx.conn.generate_id().unwrap();
        ctx.conn
            .create_window(
                ctx.screen.root_depth,
                id,
                parent,
                0,
                0,
                400,
                300,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.screen.root_visual,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        ctx.conn.map_window(id).unwrap().check().unwrap();
        id
    }

    fn set_focus(ctx: &EventContext<'_, '_>, window: Window) {
        ctx.app_ctx
            .conn
            .set_input_focus(InputFocus::PARENT, window, 0u32)
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            ctx.app_ctx
                .conn
                .get_input_focus()
                .unwrap()
                .reply()
                .unwrap()
                .focus,
            window
        );
    }

    fn visible(ctx: &EventContext<'_, '_>, source: Window) -> bool {
        let thumbnail = &ctx.eve_clients[&source];
        let viewable = ctx
            .app_ctx
            .conn
            .get_window_attributes(thumbnail.window())
            .unwrap()
            .reply()
            .unwrap()
            .map_state
            == MapState::VIEWABLE;
        assert_eq!(thumbnail.is_visible(), viewable);
        viewable
    }

    fn with_fixture(test: impl FnOnce(&mut EventContext<'_, '_>, &RustConnection, Windows)) {
        assert_eq!(std::env::var("EPM_X11_TESTS").as_deref(), Ok("1"));
        let (conn, screen_number) = x11rb::connect(None).unwrap();
        let screen = &conn.setup().roots[screen_number];
        let atoms = CachedAtoms::new(&conn).unwrap();
        let formats = CachedFormats::new(&conn, screen).unwrap();
        let app = AppContext {
            conn: &conn,
            screen,
            atoms: &atoms,
            formats: &formats,
        };
        let mut profile = Profile {
            thumbnail_enabled: true,
            thumbnail_hide_not_focused: true,
            client_minimize_on_switch: true,
            ..Profile::default()
        };
        profile.cycle_groups[0].cycle_list =
            vec![CycleSlot::Eve("Alice".into()), CycleSlot::Eve("Bob".into())];
        let mut config = DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        };
        let display = config.build_display_config();
        let font = FontRenderer::resolve_from_config(&conn, "sans-serif", 12.0).unwrap();
        let mut cycle = CycleState::new(config.profile.cycle_groups.clone());
        let mut thumbnails = HashMap::new();
        let windows = Windows {
            a: window(&app, screen.root),
            b: window(&app, screen.root),
            manager: window(&app, screen.root),
        };
        for (source, name) in [(windows.a, "Alice"), (windows.b, "Bob")] {
            cycle.add_window(Some(SourceIdentity::eve(name)), source);
            conn.change_property8(
                PropMode::REPLACE,
                source,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                format!("EVE - {name}").as_bytes(),
            )
            .unwrap()
            .check()
            .unwrap();
            conn.change_property8(
                PropMode::REPLACE,
                source,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"eve\0eve\0",
            )
            .unwrap()
            .check()
            .unwrap();
            let thumbnail = Thumbnail::new(
                &app,
                SourceKind::Eve,
                name.into(),
                None,
                source,
                &display,
                &font,
                None,
                Dimensions::new(160, 100),
                PreviewMode::default(),
                false,
            )
            .unwrap();
            thumbnails.insert(source, thumbnail);
        }
        conn.change_property8(
            PropMode::REPLACE,
            windows.manager,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"eve-preview-manager\0eve-preview-manager\0",
        )
        .unwrap()
        .check()
        .unwrap();
        conn.change_property32(
            PropMode::REPLACE,
            screen.root,
            atoms.net_client_list,
            AtomEnum::WINDOW,
            &[windows.a, windows.b, windows.manager],
        )
        .unwrap()
        .check()
        .unwrap();
        let mut session = SessionState::default();
        let mut drag = GroupDragState::default();
        let (status, _status_rx) = ipc_channel::ipc::channel().unwrap();
        // WM observation only: tests explicitly decide whether/when focus transfers.
        // SUBSTRUCTURE_NOTIFY receives activation/minimize requests without redirecting mapping.
        let (wm, _) = x11rb::connect(None).unwrap();
        wm.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )
        .unwrap()
        .check()
        .unwrap();
        let mut ctx = EventContext {
            app_ctx: &app,
            daemon_config: &mut config,
            eve_clients: &mut thumbnails,
            session_state: &mut session,
            cycle_state: &mut cycle,
            group_drag_state: &mut drag,
            status_tx: &status,
            font_renderer: &font,
            display_config: &display,
        };
        set_focus(&ctx, windows.a);
        reconcile(&mut ctx, Instant::now());
        test(&mut ctx, &wm, windows);
    }

    fn minimizations(
        ctx: &EventContext<'_, '_>,
        wm: &RustConnection,
        expected_focus: Option<Window>,
    ) -> Vec<Window> {
        wm.get_input_focus().unwrap().reply().unwrap(); // collect all preceding requests
        let mut result = Vec::new();
        while let Some(event) = wm.poll_for_event().unwrap() {
            if let Event::ClientMessage(event) = event
                && event.type_ == ctx.app_ctx.atoms.wm_change_state
                && event.data.as_data32()[0] == 3
            {
                assert_eq!(
                    Some(wm.get_input_focus().unwrap().reply().unwrap().focus),
                    expected_focus,
                    "every minimize request requires actual target focus"
                );
                result.push(event.window);
            }
        }
        result.sort_unstable();
        result
    }

    /// A real click: the release only activates the preview that received the press.
    fn click(ctx: &mut EventContext<'_, '_>, source: Window) {
        let thumbnail = &ctx.eve_clients[&source];
        let press = ButtonPressEvent {
            response_type: BUTTON_PRESS_EVENT,
            detail: 1,
            event: thumbnail.window(),
            root: ctx.app_ctx.screen.root,
            root_x: thumbnail.current_position.x,
            root_y: thumbnail.current_position.y,
            same_screen: true,
            ..Default::default()
        };
        let release = ButtonReleaseEvent {
            response_type: BUTTON_RELEASE_EVENT,
            ..press
        };
        handlers::input::handle_button_press(ctx, press).unwrap();
        handlers::input::handle_button_release(ctx, release).unwrap();
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn clicks_and_hotkeys_confirm_children_once_and_defer_manager_minimization() {
        for kind in [ActivationOrigin::Click, ActivationOrigin::Hotkey] {
            with_fixture(|ctx, wm, w| {
                if kind == ActivationOrigin::Click {
                    click(ctx, w.b);
                } else {
                    begin(ctx, w.b, Some(&SourceIdentity::eve("Bob")), 0, kind);
                }
                assert!(minimizations(ctx, wm, None).is_empty());
                assert_eq!(ctx.cycle_state.get_current_window(), Some(w.b));
                // Map restoration cannot erase the requested target's optimistic border.
                handle_event(
                    ctx,
                    Event::MapNotify(MapNotifyEvent {
                        response_type: MAP_NOTIFY_EVENT,
                        event: ctx.app_ctx.screen.root,
                        window: w.b,
                        override_redirect: false,
                        ..Default::default()
                    }),
                )
                .unwrap();
                assert!(ctx.eve_clients[&w.b].state.is_focused());
                assert!(ctx.session_state.focus.pending.is_some());
                let child = window(ctx.app_ctx, w.b);
                set_focus(ctx, child);
                reconcile(ctx, Instant::now());
                let mut expected = vec![w.a];
                if kind == ActivationOrigin::Hotkey {
                    expected.push(w.manager);
                }
                expected.sort_unstable();
                assert_eq!(minimizations(ctx, wm, Some(child)), expected);
                assert!(ctx.session_state.focus.pending.is_none());
                assert!(visible(ctx, w.a) && visible(ctx, w.b));
                reconcile(ctx, Instant::now());
                assert!(minimizations(ctx, wm, None).is_empty());
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn failed_refused_destroyed_and_late_targets_never_minimize() {
        for scenario in [
            "failed restore",
            "failed activation",
            "destroyed",
            "refused",
            "late",
            "outside",
        ] {
            with_fixture(|ctx, wm, w| {
                if matches!(scenario, "failed restore" | "failed activation") {
                    ctx.daemon_config.profile.client_minimize_on_switch =
                        scenario == "failed restore";
                    ctx.app_ctx
                        .conn
                        .destroy_window(w.b)
                        .unwrap()
                        .check()
                        .unwrap();
                }
                begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
                if scenario.starts_with("failed") {
                    assert!(ctx.session_state.focus.pending.is_none());
                } else {
                    let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
                    match scenario {
                        "destroyed" => {
                            ctx.app_ctx
                                .conn
                                .destroy_window(w.b)
                                .unwrap()
                                .check()
                                .unwrap();
                            handle_event(
                                ctx,
                                Event::DestroyNotify(DestroyNotifyEvent {
                                    response_type: DESTROY_NOTIFY_EVENT,
                                    event: w.b,
                                    window: w.b,
                                    ..Default::default()
                                }),
                            )
                            .unwrap();
                        }
                        "outside" => {
                            set_focus(ctx, w.manager);
                            reconcile(ctx, Instant::now());
                        }
                        "refused" | "late" => {
                            reconcile(ctx, deadline);
                            if scenario == "late" {
                                set_focus(ctx, w.b);
                                reconcile(ctx, deadline + PROBE_INTERVAL);
                            }
                        }
                        _ => unreachable!(),
                    }
                    assert!(ctx.session_state.focus.pending.is_none());
                }
                assert!(minimizations(ctx, wm, None).is_empty(), "{scenario}");
                assert!(visible(ctx, w.a));
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn superseding_request_keeps_cursor_when_older_target_receives_focus() {
        with_fixture(|ctx, wm, w| {
            let c = window(ctx.app_ctx, ctx.app_ctx.screen.root);
            ctx.cycle_state
                .add_window(Some(SourceIdentity::custom("Custom")), c);
            begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
            begin(ctx, c, None, 0, ActivationOrigin::Hotkey);
            set_focus(ctx, w.b);
            reconcile(ctx, Instant::now());
            assert_eq!(ctx.cycle_state.get_current_window(), Some(c));
            assert_eq!(ctx.session_state.focus.pending.as_ref().unwrap().target, c);
            assert!(minimizations(ctx, wm, None).is_empty());
            set_focus(ctx, c);
            reconcile(ctx, Instant::now());
            let mut expected = vec![w.a, w.b, w.manager];
            expected.sort_unstable();
            assert_eq!(minimizations(ctx, wm, Some(c)), expected);
            assert!(
                visible(ctx, w.a) && visible(ctx, w.b),
                "custom source without preview owns eligible focus"
            );
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn frame_preview_and_pointer_ownership_retain_visibility_without_confirming() {
        for owner in ["frame", "preview", "pointer"] {
            with_fixture(|ctx, wm, w| {
                let frame = window(ctx.app_ctx, ctx.app_ctx.screen.root);
                let focus_window = match owner {
                    "frame" => {
                        ctx.eve_clients
                            .get_mut(&w.b)
                            .unwrap()
                            .set_parent(Some(frame));
                        frame
                    }
                    "preview" => ctx.eve_clients[&w.b].window(),
                    "pointer" => 1,
                    _ => unreachable!(),
                };
                if owner == "pointer" {
                    ctx.app_ctx
                        .conn
                        .configure_window(
                            w.b,
                            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                    ctx.app_ctx
                        .conn
                        .warp_pointer(0u32, w.b, 0, 0, 0, 0, 300, 200)
                        .unwrap()
                        .check()
                        .unwrap();
                }
                begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
                set_focus(ctx, focus_window);
                reconcile(ctx, Instant::now());
                assert!(ctx.session_state.focus.pending.is_some());
                assert!(minimizations(ctx, wm, None).is_empty());
                assert!(visible(ctx, w.a));
                assert!(ctx.session_state.focus.next_probe.is_some());
                let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
                reconcile(ctx, deadline);
                assert!(ctx.session_state.focus.pending.is_none());
                assert!(visible(ctx, w.a));
                assert!(
                    ctx.session_state.focus.next_probe.is_some(),
                    "departure still needs a recheck after transaction ends"
                );
                set_focus(ctx, w.manager);
                reconcile(ctx, deadline + PROBE_INTERVAL);
                let hide_at = ctx.session_state.focus_loss_deadline.unwrap();
                reconcile(ctx, hide_at);
                assert!(!visible(ctx, w.a) && !visible(ctx, w.b));
                assert_eq!(ctx.cycle_state.get_current_window(), None);
                assert!(!ctx.eve_clients[&w.b].state.is_focused());
                // Return without a delivered source event must restore already hidden previews.
                set_focus(ctx, w.b);
                reconcile(ctx, hide_at + PROBE_INTERVAL);
                assert!(visible(ctx, w.a) && visible(ctx, w.b));
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn root_reparenting_and_unresolved_ancestry_never_confirm_or_hide_on_error() {
        with_fixture(|ctx, wm, w| {
            let root = ctx.app_ctx.screen.root;
            // Model real reparenting to root; the cached parent must be removed.
            let frame = window(ctx.app_ctx, root);
            ctx.app_ctx
                .conn
                .change_window_attributes(
                    w.b,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
                )
                .unwrap()
                .check()
                .unwrap();
            for parent in [frame, root] {
                ctx.app_ctx
                    .conn
                    .reparent_window(w.b, parent, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                assert_eq!(
                    ctx.app_ctx
                        .conn
                        .query_tree(w.b)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .parent,
                    parent
                );
                let mut saw_reparent = false;
                while let Some(event) = ctx.app_ctx.conn.poll_for_event().unwrap() {
                    if matches!(&event, Event::ReparentNotify(event) if event.window == w.b && event.parent == parent)
                    {
                        saw_reparent = true;
                        handle_event(ctx, event).unwrap();
                    }
                }
                assert!(
                    saw_reparent,
                    "dispatcher must receive real server ReparentNotify"
                );
                assert_eq!(
                    ctx.eve_clients[&w.b].parent(),
                    (parent != root).then_some(parent)
                );
            }
            for raw in [0, 1, root] {
                let resolved = focus::resolve_window(
                    ctx.app_ctx,
                    ctx.eve_clients,
                    Some(ctx.cycle_state.get_active_windows()),
                    raw,
                )
                .unwrap();
                assert!(resolved.source().is_none());
            }
            begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
            let mut child = w.b;
            for _ in 0..12 {
                child = window(ctx.app_ctx, child);
            }
            set_focus(ctx, child);
            let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
            reconcile(ctx, deadline - PROBE_INTERVAL);
            assert!(ctx.session_state.focus.observation_failed);
            assert!(visible(ctx, w.a));
            reconcile(ctx, deadline);
            assert!(ctx.session_state.focus.pending.is_none());
            assert!(visible(ctx, w.a));
            assert!(minimizations(ctx, wm, None).is_empty());
            set_focus(ctx, w.b);
            reconcile(ctx, deadline + PROBE_INTERVAL);
            assert!(!ctx.session_state.focus.observation_failed);
            assert!(
                minimizations(ctx, wm, None).is_empty(),
                "late recovery must not resurrect transaction"
            );
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn confirmed_activation_preserves_typed_remembered_exemptions_and_manual_hiding() {
        with_fixture(|ctx, wm, w| {
            let custom = window(ctx.app_ctx, ctx.app_ctx.screen.root);
            let remembered = window(ctx.app_ctx, ctx.app_ctx.screen.root);
            let no_preview = window(ctx.app_ctx, ctx.app_ctx.screen.root);
            ctx.cycle_state
                .add_window(Some(SourceIdentity::custom("Alice")), custom);
            ctx.cycle_state.add_window(None, remembered);
            ctx.cycle_state
                .add_window(Some(SourceIdentity::eve("NoPreview")), no_preview);
            ctx.session_state
                .window_last_character
                .insert(remembered, "Remembered".into());
            let mut display = ctx.display_config.clone();
            let mut exempt = crate::common::types::CharacterSettings::new(0, 0, 160, 100);
            exempt.exempt_from_minimize = true;
            display
                .custom_source_settings
                .insert("Alice".into(), exempt.clone());
            display
                .character_settings
                .insert("Remembered".into(), exempt);
            let mut context = EventContext {
                app_ctx: ctx.app_ctx,
                daemon_config: ctx.daemon_config,
                eve_clients: ctx.eve_clients,
                session_state: ctx.session_state,
                cycle_state: ctx.cycle_state,
                group_drag_state: ctx.group_drag_state,
                status_tx: ctx.status_tx,
                font_renderer: ctx.font_renderer,
                display_config: &display,
            };
            handlers::state::toggle_previews(&mut context);
            set_focus(&context, w.b); // Already focused target confirms immediately.
            begin(&mut context, w.b, None, 0, ActivationOrigin::Hotkey);
            assert!(context.session_state.focus.pending.is_none());
            let mut expected = vec![w.a, no_preview, w.manager];
            expected.sort_unstable();
            assert_eq!(minimizations(&context, wm, Some(w.b)), expected);
            assert!(!visible(&context, w.a) && !visible(&context, w.b));
            assert!(context.daemon_config.runtime_hidden);
        });
    }
    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn every_focus_mode_and_detail_uses_observed_focus_and_recovers_hidden_previews() {
        with_fixture(|ctx, wm, w| {
            for mode in [
                NotifyMode::NORMAL,
                NotifyMode::GRAB,
                NotifyMode::UNGRAB,
                NotifyMode::WHILE_GRABBED,
            ] {
                for detail in [
                    NotifyDetail::ANCESTOR,
                    NotifyDetail::VIRTUAL,
                    NotifyDetail::INFERIOR,
                    NotifyDetail::NONLINEAR,
                    NotifyDetail::NONLINEAR_VIRTUAL,
                    NotifyDetail::POINTER,
                    NotifyDetail::POINTER_ROOT,
                    NotifyDetail::NONE,
                ] {
                    handlers::state::hide_after_focus_loss(ctx);
                    assert!(!visible(ctx, w.a));
                    // A real source owns focus; misleading event metadata cannot hide it.
                    handle_event(
                        ctx,
                        Event::FocusOut(FocusOutEvent {
                            response_type: FOCUS_OUT_EVENT,
                            event: w.b,
                            mode,
                            detail,
                            ..Default::default()
                        }),
                    )
                    .unwrap();
                    assert!(visible(ctx, w.a) && visible(ctx, w.b));
                    assert!(ctx.session_state.focus_loss_deadline.is_none());
                    assert_eq!(ctx.cycle_state.get_current_window(), Some(w.a));
                    handle_event(
                        ctx,
                        Event::FocusIn(FocusInEvent {
                            response_type: FOCUS_IN_EVENT,
                            event: w.b,
                            mode,
                            detail,
                            ..Default::default()
                        }),
                    )
                    .unwrap();
                    assert_eq!(ctx.cycle_state.get_current_window(), Some(w.a));
                }
            }
            assert!(minimizations(ctx, wm, None).is_empty());
        });
    }
    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn activation_from_original_outside_owner_can_complete() {
        with_fixture(|ctx, wm, w| {
            set_focus(ctx, w.manager);
            begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
            assert!(ctx.session_state.focus.pending.is_some());
            assert!(minimizations(ctx, wm, None).is_empty());
            set_focus(ctx, w.b);
            reconcile(ctx, Instant::now());
            let mut expected = vec![w.a, w.manager];
            expected.sort_unstable();
            assert_eq!(minimizations(ctx, wm, Some(w.b)), expected);
            assert!(visible(ctx, w.a) && visible(ctx, w.b));
        });
    }

    /// Dispatch every event already delivered to the daemon connection.
    fn dispatch_delivered(ctx: &mut EventContext<'_, '_>) {
        // The round trip guarantees events generated before it have arrived.
        ctx.app_ctx.conn.get_input_focus().unwrap().reply().unwrap();
        while let Some(event) = ctx.app_ctx.conn.poll_for_event().unwrap() {
            handle_event(ctx, event).unwrap();
        }
    }

    fn observed_owner(ctx: &EventContext<'_, '_>) -> FocusOwner {
        ctx.session_state.focus.last_observation.unwrap().owner
    }

    fn unmap(root: Window, window: Window) -> Event {
        Event::UnmapNotify(UnmapNotifyEvent {
            response_type: UNMAP_NOTIFY_EVENT,
            event: root,
            window,
            from_configure: false,
            ..Default::default()
        })
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn requested_borders_preserve_minimized_state_until_restore_is_verified() {
        for scenario in ["refused", "stale map", "failed", "superseded", "restored"] {
            with_fixture(|ctx, wm, w| {
                let root = ctx.app_ctx.screen.root;
                ctx.app_ctx.conn.unmap_window(w.b).unwrap().check().unwrap();
                ctx.eve_clients
                    .get_mut(&w.b)
                    .unwrap()
                    .minimized(ctx.display_config, ctx.font_renderer)
                    .unwrap();
                if scenario == "failed" {
                    // The restore request itself fails, cancelling the transaction in `begin`.
                    ctx.app_ctx
                        .conn
                        .destroy_window(w.b)
                        .unwrap()
                        .check()
                        .unwrap();
                }
                if scenario != "restored" {
                    // The WM receives the MapRequest but never maps the source.
                    wm.change_window_attributes(
                        root,
                        &ChangeWindowAttributesAux::new().event_mask(
                            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                        ),
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                }
                begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
                assert!(
                    ctx.eve_clients[&w.b].state.is_minimized(),
                    "{scenario}: a request cannot promote minimized rendering"
                );
                if scenario != "failed" {
                    assert!(!ctx.eve_clients[&w.a].state.is_focused());
                }
                match scenario {
                    "failed" => {}
                    "stale map" => {
                        // An old MapNotify is not a verified restore while the source is unmapped.
                        handle_event(
                            ctx,
                            Event::MapNotify(MapNotifyEvent {
                                response_type: MAP_NOTIFY_EVENT,
                                event: root,
                                window: w.b,
                                override_redirect: false,
                                ..Default::default()
                            }),
                        )
                        .unwrap();
                        assert!(
                            ctx.eve_clients[&w.b].state.is_minimized(),
                            "a stale MapNotify must keep minimized rendering"
                        );
                        let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
                        reconcile(ctx, deadline);
                    }
                    "refused" => {
                        let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
                        reconcile(ctx, deadline);
                        let map_state = ctx
                            .app_ctx
                            .conn
                            .get_window_attributes(w.b)
                            .unwrap()
                            .reply()
                            .unwrap()
                            .map_state;
                        assert_eq!(map_state, MapState::UNMAPPED);
                    }
                    "superseded" => {
                        let c = window(ctx.app_ctx, root);
                        ctx.cycle_state
                            .add_window(Some(SourceIdentity::custom("Custom")), c);
                        begin(ctx, c, None, 0, ActivationOrigin::Hotkey);
                        let deadline = ctx.session_state.focus.pending.as_ref().unwrap().deadline;
                        reconcile(ctx, deadline);
                    }
                    "restored" => {
                        // Without redirection the restore request maps the source directly;
                        // the verified map refresh then shows the requested border.
                        handle_event(
                            ctx,
                            Event::MapNotify(MapNotifyEvent {
                                response_type: MAP_NOTIFY_EVENT,
                                event: root,
                                window: w.b,
                                override_redirect: false,
                                ..Default::default()
                            }),
                        )
                        .unwrap();
                        assert!(ctx.eve_clients[&w.b].state.is_focused());
                        set_focus(ctx, w.b);
                        reconcile(ctx, Instant::now());
                        let mut expected = vec![w.a, w.manager];
                        expected.sort_unstable();
                        assert_eq!(minimizations(ctx, wm, Some(w.b)), expected);
                        return;
                    }
                    _ => unreachable!(),
                }
                assert!(ctx.session_state.focus.pending.is_none());
                assert!(minimizations(ctx, wm, None).is_empty(), "{scenario}");
                assert!(
                    ctx.eve_clients[&w.b].state.is_minimized(),
                    "{scenario}: cancellation must retain minimized rendering"
                );
                assert!(ctx.eve_clients[&w.a].state.is_focused());
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn pointer_root_ownership_reaches_sources_through_frames_and_deep_children() {
        for layout in ["nested frame", "deep child"] {
            with_fixture(|ctx, wm, w| {
                let root = ctx.app_ctx.screen.root;
                let pointer_target = if layout == "nested frame" {
                    // root -> outer frame -> wrapper (cached parent) -> source
                    let frame = window(ctx.app_ctx, root);
                    let wrapper = window(ctx.app_ctx, frame);
                    ctx.app_ctx
                        .conn
                        .reparent_window(w.b, wrapper, 0, 0)
                        .unwrap()
                        .check()
                        .unwrap();
                    ctx.eve_clients
                        .get_mut(&w.b)
                        .unwrap()
                        .set_parent(Some(wrapper));
                    w.b
                } else {
                    // More nested input children than either traversal bound.
                    ctx.app_ctx
                        .conn
                        .configure_window(
                            w.b,
                            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                    let mut child = w.b;
                    for _ in 0..40 {
                        child = window(ctx.app_ctx, child);
                    }
                    child
                };
                ctx.app_ctx
                    .conn
                    .warp_pointer(0u32, pointer_target, 0, 0, 0, 0, 10, 10)
                    .unwrap()
                    .check()
                    .unwrap();
                set_focus(ctx, 1);
                let now = Instant::now();
                reconcile(ctx, now);
                reconcile(ctx, now + HIDE_DELAY);
                let observation = ctx.session_state.focus.last_observation.unwrap();
                assert!(observation.pointer_root, "{layout}");
                assert_eq!(observation.owner.source(), Some(w.b), "{layout}");
                assert!(visible(ctx, w.a) && visible(ctx, w.b), "{layout}");
                // Pointer-derived ownership never confirms activation.
                begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
                reconcile(ctx, Instant::now());
                assert!(ctx.session_state.focus.pending.is_some(), "{layout}");
                assert!(minimizations(ctx, wm, None).is_empty(), "{layout}");
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn delayed_focus_reply_after_deadline_cancels_instead_of_confirming() {
        with_fixture(|ctx, wm, w| {
            begin(ctx, w.b, None, 0, ActivationOrigin::Hotkey);
            assert!(minimizations(ctx, wm, None).is_empty());
            set_focus(ctx, w.b);
            wm.grab_server().unwrap().check().unwrap();
            let deadline = Instant::now() + Duration::from_millis(50);
            ctx.session_state.focus.pending.as_mut().unwrap().deadline = deadline;
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(100));
                    wm.ungrab_server().unwrap().check().unwrap();
                });
                // The focus query blocks until the grab ends, after the deadline.
                reconcile(ctx, Instant::now());
            });
            assert!(Instant::now() > deadline);
            assert!(ctx.session_state.focus.pending.is_none());
            assert!(
                minimizations(ctx, wm, None).is_empty(),
                "a reply arriving after the deadline must time out rather than confirm"
            );
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn root_focus_events_track_none_and_root_without_idle_polling() {
        with_fixture(|ctx, _wm, w| {
            let root = ctx.app_ctx.screen.root;
            // Pre-existing bits on every screen root must survive the merge.
            let seeded = EventMask::PROPERTY_CHANGE | EventMask::COLOR_MAP_CHANGE;
            for screen in &ctx.app_ctx.conn.setup().roots {
                ctx.app_ctx
                    .conn
                    .change_window_attributes(
                        screen.root,
                        &ChangeWindowAttributesAux::new().event_mask(seeded),
                    )
                    .unwrap()
                    .check()
                    .unwrap();
            }
            focus::select_root_focus_changes(ctx.app_ctx.conn).unwrap();
            for screen in &ctx.app_ctx.conn.setup().roots {
                let mask = ctx
                    .app_ctx
                    .conn
                    .get_window_attributes(screen.root)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .your_event_mask;
                assert!(
                    mask.contains(seeded | EventMask::FOCUS_CHANGE),
                    "root {} kept {mask:?}",
                    screen.root
                );
            }
            dispatch_delivered(ctx);
            // No X client focused (for example, a native Wayland app on wlroots).
            set_focus(ctx, 0);
            dispatch_delivered(ctx);
            assert_eq!(observed_owner(ctx), FocusOwner::None);
            assert!(ctx.session_state.focus.pending.is_none());
            assert_eq!(
                ctx.session_state.focus.next_probe, None,
                "no idle polling at None"
            );
            let hide_at = ctx.session_state.focus_loss_deadline.unwrap();
            reconcile(ctx, hide_at);
            assert!(!visible(ctx, w.a) && !visible(ctx, w.b));
            // None -> outside application: only root is notified.
            set_focus(ctx, w.manager);
            dispatch_delivered(ctx);
            assert_eq!(observed_owner(ctx), FocusOwner::Outside(w.manager));
            // Outside -> root window.
            set_focus(ctx, root);
            dispatch_delivered(ctx);
            assert_eq!(observed_owner(ctx), FocusOwner::Root);
            assert_eq!(
                ctx.session_state.focus.next_probe, None,
                "no idle polling at root"
            );
            // Root -> source: recovery arrives through root's FocusOut alone.
            set_focus(ctx, w.b);
            dispatch_delivered(ctx);
            assert_eq!(observed_owner(ctx), FocusOwner::Source(w.b));
            assert!(visible(ctx, w.a) && visible(ctx, w.b));
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn only_focus_relevant_unmaps_and_destroys_requery_focus() {
        with_fixture(|ctx, _wm, w| {
            let root = ctx.app_ctx.screen.root;
            assert_eq!(observed_owner(ctx), FocusOwner::Source(w.a));
            // Focus moves without delivering any focus event to the daemon.
            set_focus(ctx, w.b);
            // Our own hidden preview and unrelated windows cannot change ownership.
            let preview = ctx.eve_clients[&w.a].window();
            handle_event(ctx, unmap(root, preview)).unwrap();
            let unrelated = window(ctx.app_ctx, root);
            handle_event(ctx, unmap(root, unrelated)).unwrap();
            handle_event(
                ctx,
                Event::DestroyNotify(DestroyNotifyEvent {
                    response_type: DESTROY_NOTIFY_EVENT,
                    event: root,
                    window: unrelated,
                    ..Default::default()
                }),
            )
            .unwrap();
            assert_eq!(observed_owner(ctx), FocusOwner::Source(w.a));
            // A tracked source's unmap re-queries.
            handle_event(ctx, unmap(root, w.a)).unwrap();
            assert_eq!(observed_owner(ctx), FocusOwner::Source(w.b));
            // So does unmapping whichever window held the last observed focus.
            let outside = window(ctx.app_ctx, root);
            set_focus(ctx, outside);
            reconcile(ctx, Instant::now());
            assert_eq!(observed_owner(ctx), FocusOwner::Outside(outside));
            set_focus(ctx, w.b);
            handle_event(ctx, unmap(root, outside)).unwrap();
            assert_eq!(observed_owner(ctx), FocusOwner::Source(w.b));
        });
    }
}
