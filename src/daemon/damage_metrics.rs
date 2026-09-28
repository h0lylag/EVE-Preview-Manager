//! Opt-in aggregate observations of damage handling, not completed server/GPU frames.

use std::time::{Duration, Instant};

use super::thumbnail::DamageUpdate;
use crate::common::types::SourceKind;

const REPORT_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    notifications: u64,
    hidden: u64,
    static_preview: u64,
    minimized: u64,
    live_capture_requests_queued: u64,
    live_unavailable: u64,
    errors: u64,
    handler_time: Duration,
    expose_update_attempts: u64,
    expose_errors: u64,
}

#[derive(Debug)]
pub(super) struct DamageMetrics {
    since: Instant,
    counts: Counts,
}

impl DamageMetrics {
    // The application's tracing filter is fixed at startup. Disabled previews initialize no
    // counter state and never read the clock on the damage path.
    pub(super) fn new_if_enabled() -> Option<Self> {
        tracing::enabled!(target: "epm_damage_metrics", tracing::Level::DEBUG)
            .then(|| Self::new(Instant::now()))
    }

    fn new(now: Instant) -> Self {
        Self {
            since: now,
            counts: Counts::default(),
        }
    }

    pub(super) fn record_damage(&mut self, outcome: Option<DamageUpdate>, elapsed: Duration) {
        let counts = &mut self.counts;
        counts.notifications += 1;
        counts.handler_time = counts.handler_time.saturating_add(elapsed);
        match outcome {
            Some(DamageUpdate::Hidden) => counts.hidden += 1,
            Some(DamageUpdate::Static) => counts.static_preview += 1,
            Some(DamageUpdate::Minimized) => counts.minimized += 1,
            Some(DamageUpdate::LiveCaptured) => counts.live_capture_requests_queued += 1,
            Some(DamageUpdate::LiveUnavailable) => counts.live_unavailable += 1,
            None => counts.errors += 1,
        }
    }

    pub(super) fn record_expose(&mut self, failed: bool) {
        self.counts.expose_update_attempts += 1;
        self.counts.expose_errors += u64::from(failed);
    }

    fn take_interval(&mut self, now: Instant) -> Option<(Duration, Counts)> {
        let interval = now.duration_since(self.since);
        if interval < REPORT_INTERVAL {
            return None;
        }
        self.since = now;
        Some((interval, std::mem::take(&mut self.counts)))
    }

    pub(super) fn report_if_due(&mut self, kind: SourceKind, source_window: u32) {
        let Some((interval, counts)) = self.take_interval(Instant::now()) else {
            return;
        };
        tracing::debug!(target: "epm_damage_metrics",
            source_window, source_kind = ?kind,
            interval_ms = interval.as_millis() as u64,
            notifications = counts.notifications,
            skipped_hidden = counts.hidden,
            skipped_static = counts.static_preview,
            skipped_minimized = counts.minimized,
            live_capture_requests_queued = counts.live_capture_requests_queued,
            live_unavailable = counts.live_unavailable,
            errors = counts.errors,
            handler_us = counts.handler_time.as_micros() as u64,
            expose_update_attempts = counts.expose_update_attempts,
            expose_errors = counts.expose_errors,
            "Damage pipeline interval");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_and_errors_account_for_every_notification() {
        let start = Instant::now();
        let mut metrics = DamageMetrics::new(start);
        for outcome in [
            Some(DamageUpdate::Hidden),
            Some(DamageUpdate::Static),
            Some(DamageUpdate::Minimized),
            Some(DamageUpdate::LiveCaptured),
            Some(DamageUpdate::LiveUnavailable),
            None,
        ] {
            metrics.record_damage(outcome, Duration::from_micros(7));
        }
        metrics.record_expose(false);
        metrics.record_expose(true);
        let (interval, counts) = metrics.take_interval(start + REPORT_INTERVAL).unwrap();
        assert_eq!(interval, REPORT_INTERVAL);
        assert_eq!(counts.notifications, 6);
        assert_eq!(counts.hidden, 1);
        assert_eq!(counts.static_preview, 1);
        assert_eq!(counts.minimized, 1);
        assert_eq!(counts.live_capture_requests_queued, 1);
        assert_eq!(counts.live_unavailable, 1);
        assert_eq!(counts.errors, 1);
        assert_eq!(counts.handler_time, Duration::from_micros(42));
        assert_eq!(counts.expose_update_attempts, 2);
        assert_eq!(counts.expose_errors, 1);
        assert_eq!(metrics.counts, Counts::default());
    }

    #[test]
    fn heartbeat_reports_idle_sources_and_resets_its_interval() {
        let start = Instant::now();
        let mut metrics = DamageMetrics::new(start);
        assert!(
            metrics
                .take_interval(start + REPORT_INTERVAL - Duration::from_nanos(1))
                .is_none()
        );
        let report_at = start + Duration::from_secs(12);
        assert_eq!(
            metrics.take_interval(report_at),
            Some((Duration::from_secs(12), Counts::default()))
        );
        metrics.record_damage(Some(DamageUpdate::LiveCaptured), Duration::from_micros(3));
        assert!(
            metrics
                .take_interval(report_at + Duration::from_secs(9))
                .is_none()
        );
        let (_, counts) = metrics.take_interval(report_at + REPORT_INTERVAL).unwrap();
        assert_eq!(counts.notifications, 1);
        assert_eq!(
            metrics.take_interval(report_at + REPORT_INTERVAL * 2),
            Some((REPORT_INTERVAL, Counts::default()))
        );
    }

    #[test]
    fn only_an_enabled_metrics_target_initializes_counters() {
        for (filter, expected) in [
            ("info", false),
            ("info,eve_preview_manager=debug", false),
            ("info,epm_damage_metrics=debug", true),
        ] {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
                .with_writer(std::io::sink)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                assert_eq!(
                    DamageMetrics::new_if_enabled().is_some(),
                    expected,
                    "{filter}"
                );
            });
        }
    }
}
