//! The channel's one line on a screen it does not own.
//!
//! `riabuild_ui::StatusBar` knows how to put a line on a row and take it off
//! again. What it cannot know is that something underneath keeps painting over
//! it: the supervisor runs beside a mosh session with a full-screen Claude Code
//! in it, and the moment that program redraws row two the bar is gone with no
//! error and nobody told. So the line has to be drawn again, on a tick, for as
//! long as it is meant to be up.
//!
//! That is all this is — a bar, and the task that keeps redrawing it. It lives
//! in `channel` rather than in `ui` because the redrawing needs a runtime and
//! `ui` deliberately has none: everything else in that crate is a `println!`.

use riabuild_ui::StatusBar;
use std::sync::Arc;
use std::time::Duration;

/// How often the line is drawn again.
///
/// Fast enough that a warning painted over by a repaint comes back before the
/// developer has finished reading the sentence it interrupted, slow enough to
/// be nothing on a terminal's workload — one short write, and only while there
/// is something to say. A clear bar redraws nothing at all.
const REPAINT: Duration = Duration::from_secs(2);

/// A status bar, and the task keeping it on the screen.
pub struct StatusLine {
    bar: Arc<StatusBar>,
    painting: tokio::task::JoinHandle<()>,
}

impl StatusLine {
    /// Opens the developer's terminal and starts redrawing whatever is put on
    /// the bar.
    ///
    /// Never fails: a run with no terminal — under `--quiet`, in CI, in a test
    /// — gets a disabled bar whose every method is a no-op, and the supervisor
    /// prints the ordinary way instead.
    pub fn start(quiet: bool) -> StatusLine {
        let bar = Arc::new(StatusBar::on_second_line(quiet));
        let painting = tokio::spawn({
            let bar = Arc::clone(&bar);
            async move {
                loop {
                    tokio::time::sleep(REPAINT).await;
                    bar.repaint();
                }
            }
        });
        StatusLine { bar, painting }
    }

    /// The bar itself, for the supervisor to write on.
    pub fn bar(&self) -> Arc<StatusBar> {
        Arc::clone(&self.bar)
    }

    /// Takes the line off the screen and stops redrawing it.
    ///
    /// In that order, and it matters: `clear` empties the bar under the lock
    /// every paint is made under, so a repaint already in flight finds nothing
    /// to draw rather than putting a dead warning back on a terminal riabuild
    /// is about to print its own output to.
    pub fn stop(self) {
        self.bar.clear();
        self.painting.abort();
    }
}
