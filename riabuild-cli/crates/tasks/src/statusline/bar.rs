//! How full the context window is: `█████░░░░░ 54%`, green → yellow → orange →
//! blinking red 💀.
//!
//! The bar measures the **usable** window rather than the whole one. Claude Code
//! reserves a buffer for auto-compaction — 16.5% by default, and
//! `CLAUDE_CODE_AUTO_COMPACT_WINDOW` where a session sets one — so a bar drawn
//! against the raw `remaining_percentage` would sit at 83% at the moment
//! compaction actually fires. Reading 100% when the session compacts is the
//! whole point of the number.

use serde_json::Value;

/// Claude Code's default auto-compaction buffer, as a percentage of the window.
const DEFAULT_BUFFER_PCT: f64 = 16.5;

/// The window Claude Code assumes when the payload names none.
const DEFAULT_TOTAL_TOKENS: f64 = 1_000_000.0;

/// The bar, with a leading space, or nothing at all when Claude Code sent no
/// window data — which it does before the first API response of a session.
pub(super) fn draw(payload: &Value, auto_compact_window: Option<u64>) -> String {
    let window = payload.get("context_window");
    let Some(remaining) = window
        .and_then(|window| window.get("remaining_percentage"))
        .and_then(Value::as_f64)
    else {
        return String::new();
    };

    let used = used_pct(remaining, total_tokens(window), auto_compact_window);
    let filled = (used / 10.0).floor() as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled));
    let used = used as u32;

    match used {
        0..50 => format!(" \x1b[32m{bar} {used}%\x1b[0m"),
        50..65 => format!(" \x1b[33m{bar} {used}%\x1b[0m"),
        65..80 => format!(" \x1b[38;5;208m{bar} {used}%\x1b[0m"),
        // 💀, blinking. The window is nearly gone and a colour alone has stopped
        // being noticed by then.
        _ => format!(" \x1b[5;31m\u{1F480} {bar} {used}%\x1b[0m"),
    }
}

fn total_tokens(window: Option<&Value>) -> f64 {
    window
        .and_then(|window| window.get("total_tokens"))
        .and_then(Value::as_f64)
        .filter(|total| *total > 0.0)
        .unwrap_or(DEFAULT_TOTAL_TOKENS)
}

/// How much of the *usable* window is gone, 0–100 and never outside it.
fn used_pct(remaining: f64, total_tokens: f64, auto_compact_window: Option<u64>) -> f64 {
    let buffer = match auto_compact_window {
        Some(window) if window > 0 => {
            (100.0 - (window as f64 / total_tokens) * 100.0).clamp(0.0, 100.0)
        }
        _ => DEFAULT_BUFFER_PCT,
    };
    // A buffer of the whole window leaves nothing usable, so the honest reading
    // is "full" rather than the division by zero the arithmetic below would be.
    if buffer >= 100.0 {
        return 100.0;
    }
    let usable_remaining = (((remaining - buffer) / (100.0 - buffer)) * 100.0).max(0.0);
    (100.0 - usable_remaining).round().clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn drawn(remaining: f64) -> String {
        draw(
            &json!({ "context_window": { "remaining_percentage": remaining } }),
            None,
        )
    }

    /// A fresh session reads empty, not 17% used. Measuring the raw window
    /// instead of the usable one is the bug this pins.
    #[test]
    fn a_full_window_reads_empty() {
        assert!(drawn(100.0).contains("0%"), "{:?}", drawn(100.0));
        assert!(drawn(100.0).contains("░░░░░░░░░░"), "{:?}", drawn(100.0));
    }

    /// And the moment auto-compaction fires reads 100%, which is the whole
    /// reason the buffer is subtracted at all.
    #[test]
    fn the_compaction_threshold_reads_full() {
        let bar = drawn(DEFAULT_BUFFER_PCT);

        assert!(bar.contains("100%"), "{bar:?}");
        assert!(bar.contains("██████████"), "{bar:?}");
    }

    /// Claude Code sends no window data before the first API response of a
    /// session, and a bar drawn from nothing would say `0%` about a session it
    /// knows nothing about.
    #[test]
    fn a_payload_with_no_window_draws_no_bar() {
        assert_eq!(draw(&json!({}), None), "");
        assert_eq!(draw(&json!({ "context_window": {} }), None), "");
    }

    /// A session that sets its own auto-compaction window moves the threshold,
    /// and the bar has to move with it or it is measuring a window that is not
    /// this session's.
    #[test]
    fn a_session_that_sets_its_own_compaction_window_is_measured_against_that() {
        let payload = json!({
            "context_window": { "remaining_percentage": 50.0, "total_tokens": 1_000_000 }
        });

        // 800k usable of a 1M window: a 20% buffer, so half the raw window left
        // is 37.5% of the usable one left, and 62.5% of it gone.
        let bar = draw(&payload, Some(800_000));

        assert!(bar.contains("63%"), "{bar:?}");
        // And the default buffer would have called the same payload 60% —
        // which is the reading this exists to stop.
        assert!(draw(&payload, None).contains("60%"));
    }

    /// Every reading stays inside the bar. A payload claiming more remaining
    /// than exists must not draw eleven blocks or a negative percentage.
    #[test]
    fn no_payload_can_draw_outside_the_bar() {
        for remaining in [-40.0, 0.0, 16.5, 50.0, 100.0, 400.0] {
            let bar = drawn(remaining);
            let blocks = bar.chars().filter(|c| *c == '█' || *c == '░').count();
            assert_eq!(blocks, 10, "{remaining} drew {bar:?}");
            assert!(!bar.contains('-'), "{remaining} drew {bar:?}");
        }
    }

    /// The colour says how urgent it is, and the last band adds a glyph because
    /// a colour alone stops being noticed.
    #[test]
    fn the_bar_gets_louder_as_the_window_fills() {
        // 0%, ~57%, ~70%, ~100% used.
        assert!(drawn(100.0).contains("\x1b[32m"));
        assert!(drawn(50.0).contains("\x1b[33m"));
        assert!(drawn(40.0).contains("\x1b[38;5;208m"));
        assert!(drawn(16.5).contains("\u{1F480}"));
    }
}
