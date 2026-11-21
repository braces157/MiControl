//! Pure ChargeGuard state machine.
//!
//! Xiaomi EC resets or forgets the WMI charge limit register across sleep/hibernation
//! and AC/battery transitions. ChargeGuard ensures the user's intended charge threshold
//! is re-armed cleanly:
//! - On startup
//! - On suspend / shutdown (immediately, so laptop doesn't charge to 100% while off)
//! - On resume / power-source change (after debounce)
//! - Reconciles external changes (< 100%) on UI open without ever letting transient 100%
//!   readbacks disarm the user's intent.

use crate::config::AppConfig;
use crate::protocol::ChargeLimit;

/// Decision returned by `ChargeGuard` for a hardware synchronization event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardAction {
    /// No firmware write needed.
    NoOp,
    /// Write this threshold to MIFS (using off -> 80ms -> target reset sequence if `< 100%`).
    Apply { target: ChargeLimit, urgent: bool },
}

/// Evaluate whether firmware needs re-arming at startup, resume, power change, or pre-suspend.
pub fn evaluate_rearm(
    cfg: &AppConfig,
    firmware_current: Option<ChargeLimit>,
    urgent_before_sleep: bool,
) -> GuardAction {
    let desired = cfg.desired_firmware_limit();
    if !desired.is_care_active() {
        return GuardAction::NoOp;
    }

    // If EC already holds the desired care limit in this session, skip redundant writes.
    if firmware_current == Some(desired) {
        return GuardAction::NoOp;
    }

    GuardAction::Apply {
        target: desired,
        urgent: urgent_before_sleep,
    }
}

/// Reconcile an external firmware readback (when the user opens the flyout or menu).
///
/// Rules (verified against TM24* EC behavior):
/// - If firmware is silent (`None`), keep config untouched.
/// - If `travel_mode` is active, never overwrite `charge_care` / `charge_limit`.
/// - Never adopt `100%` (could be a transient post-resume EC reset racing ChargeGuard).
/// - Adopt any valid `< 100%` firmware threshold (only an explicit external `SET` creates it).
///
/// Returns `true` if `cfg` was updated.
pub fn reconcile_from_firmware(cfg: &mut AppConfig, firmware_current: Option<ChargeLimit>) -> bool {
    if cfg.travel_mode {
        return false;
    }
    let Some(fw) = firmware_current else {
        return false;
    };
    if !fw.is_care_active() {
        return false;
    }
    if !cfg.charge_care || cfg.charge_limit != fw {
        cfg.charge_care = true;
        cfg.charge_limit = fw;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rearms_when_firmware_drops_to_100_or_unknown() {
        let cfg = AppConfig {
            charge_care: true,
            charge_limit: ChargeLimit::P80,
            travel_mode: false,
            ..Default::default()
        };
        assert_eq!(
            evaluate_rearm(&cfg, Some(ChargeLimit::P100), false),
            GuardAction::Apply {
                target: ChargeLimit::P80,
                urgent: false,
            }
        );
        assert_eq!(
            evaluate_rearm(&cfg, None, true),
            GuardAction::Apply {
                target: ChargeLimit::P80,
                urgent: true,
            }
        );
        assert_eq!(evaluate_rearm(&cfg, Some(ChargeLimit::P80), false), GuardAction::NoOp);
    }

    #[test]
    fn noop_in_travel_mode_or_when_care_disabled() {
        let mut cfg = AppConfig {
            charge_care: true,
            charge_limit: ChargeLimit::P80,
            travel_mode: true,
            ..Default::default()
        };
        assert_eq!(evaluate_rearm(&cfg, Some(ChargeLimit::P100), false), GuardAction::NoOp);
        cfg.travel_mode = false;
        cfg.charge_care = false;
        assert_eq!(evaluate_rearm(&cfg, Some(ChargeLimit::P100), true), GuardAction::NoOp);
    }

    #[test]
    fn reconcile_ignores_transient_100_but_adopts_external_care_limit() {
        let mut cfg = AppConfig {
            charge_care: true,
            charge_limit: ChargeLimit::P80,
            travel_mode: false,
            ..Default::default()
        };
        // Transient 100% must NOT disarm charge care!
        assert!(!reconcile_from_firmware(&mut cfg, Some(ChargeLimit::P100)));
        assert!(cfg.charge_care);
        assert_eq!(cfg.charge_limit, ChargeLimit::P80);

        // External tool switched to 60% -> adopt it
        assert!(reconcile_from_firmware(&mut cfg, Some(ChargeLimit::P60)));
        assert_eq!(cfg.charge_limit, ChargeLimit::P60);

        // In travel mode -> ignore
        cfg.travel_mode = true;
        assert!(!reconcile_from_firmware(&mut cfg, Some(ChargeLimit::P50)));
        assert_eq!(cfg.charge_limit, ChargeLimit::P60);
    }
}
