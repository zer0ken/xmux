//! Toasts and the history: what xmux tells the user about work that finished, and the
//! record of every such report and every background event.
//!
//! A toast reports the result of work the user started. A background event (a host that
//! stopped answering while nobody asked it anything) is recorded without a toast, because
//! an interruption the user did not cause would pull attention from the terminal they are
//! working in. Both land in the history, which `prefix m` opens.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// How a report line reads, which decides its glyph, its colour, and how long a toast
/// carrying it stays up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    /// Work that succeeded.
    Success,
    /// A fact that is neither a success nor a failure.
    Info,
    /// Something the user should look at that is not a failure of the work itself.
    Warning,
    /// Work that failed.
    Error,
}

impl Level {
    /// The one-cell glyph the line leads with, from the terminal-safe vocabulary.
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Level::Success => "✓",
            Level::Info => "·",
            Level::Warning => "▲",
            Level::Error => "✗",
        }
    }

    /// A warning or an error stays until the user dismisses it: it asks for action, and
    /// a report that left on its own could leave before it was read.
    pub(crate) fn sticky(self) -> bool {
        matches!(self, Level::Warning | Level::Error)
    }
}

/// One report line: its level and its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    pub(crate) level: Level,
    pub(crate) text: String,
}

impl Note {
    pub(crate) fn new(level: Level, text: impl Into<String>) -> Self {
        Note {
            level,
            text: text.into(),
        }
    }
}

/// A toast on screen: a titled box of report lines floating over the terminal view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Toast {
    pub(crate) id: u64,
    /// What the report is about (a host, a re-scan), drawn in the box's top border.
    pub(crate) title: String,
    pub(crate) notes: Vec<Note>,
    pub(crate) shown: Instant,
    /// When the toast takes itself down, or `None` for one that waits to be dismissed.
    pub(crate) until: Option<Instant>,
}

impl Toast {
    /// The share of the toast's life still ahead at `now`, from 1 down to 0, or `None`
    /// for a toast that waits to be dismissed.
    pub(crate) fn remaining(&self, now: Instant) -> Option<f32> {
        let until = self.until?;
        let life = until.saturating_duration_since(self.shown).as_secs_f32();
        if life <= 0.0 {
            return Some(0.0);
        }
        let left = until.saturating_duration_since(now).as_secs_f32();
        Some((left / life).clamp(0.0, 1.0))
    }
}

/// One history record: one report line, when it happened, and what it was about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) at: Instant,
    pub(crate) title: String,
    pub(crate) note: Note,
}

/// How long a toast with no warning or error stays up.
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(5);

/// The smallest unit the history writes an age in, and so how often its open popup is
/// repainted for the ages to move.
const AGE_STEP: Duration = Duration::from_secs(1);

/// How many toasts stand on screen at once. The newest toast always shows: a further one
/// takes the place of the oldest older toast that would leave by itself, or of the oldest
/// toast when every older one waits to be dismissed; the history keeps all of them either
/// way.
pub(crate) const TOAST_STACK: usize = 3;

/// How many records the history holds. A full history drops its oldest success or info
/// record first, so a burst of routine reports cannot push a failure out of it.
pub(crate) const HISTORY_CAP: usize = 200;

/// The toasts on screen and the history behind them.
#[derive(Debug)]
pub(crate) struct Notifications {
    pub(crate) toasts: Vec<Toast>,
    pub(crate) history: VecDeque<Entry>,
    /// Whether results are shown as toasts (`[ui] notifications`). Off, they still land
    /// in the history.
    pub(crate) toasts_enabled: bool,
    /// The clock as of the last tick, so rendering reads the toasts' remaining life and
    /// the history's ages without reading the clock itself.
    pub(crate) now: Option<Instant>,
    /// Whether the last tick changed what the toasts or the open history show, which the
    /// tick is the only wake for.
    pub(crate) repaint: bool,
    /// The tick that last advanced the open history's ages, or `None` while it is closed.
    aged: Option<Instant>,
    next_id: u64,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            toasts: Vec::new(),
            history: VecDeque::new(),
            toasts_enabled: true,
            now: None,
            repaint: false,
            aged: None,
            next_id: 0,
        }
    }
}

impl Notifications {
    /// Reports the result of work the user started: a toast, and the history.
    pub(crate) fn toast(&mut self, title: impl Into<String>, notes: Vec<Note>) {
        self.toast_at(Instant::now(), title, notes);
    }

    /// [`Self::toast`] at a given instant.
    pub(crate) fn toast_at(&mut self, now: Instant, title: impl Into<String>, notes: Vec<Note>) {
        if notes.is_empty() {
            return;
        }
        let title = title.into();
        self.record_at(now, &title, &notes);
        if !self.toasts_enabled {
            return;
        }
        let sticky = notes.iter().any(|n| n.level.sticky());
        self.next_id += 1;
        self.toasts.push(Toast {
            id: self.next_id,
            title,
            notes,
            shown: now,
            until: (!sticky).then(|| now + TOAST_TTL),
        });
        if self.toasts.len() > TOAST_STACK {
            let older = &self.toasts[..self.toasts.len() - 1];
            let drop = older.iter().position(|t| t.until.is_some()).unwrap_or(0);
            self.toasts.remove(drop);
        }
        self.now.get_or_insert(now);
    }

    /// Records a background event: the history only, no toast.
    pub(crate) fn record(&mut self, title: impl Into<String>, notes: Vec<Note>) {
        self.record_at(Instant::now(), &title.into(), &notes);
    }

    fn record_at(&mut self, now: Instant, title: &str, notes: &[Note]) {
        for note in notes {
            if self.history.len() >= HISTORY_CAP {
                let drop = self
                    .history
                    .iter()
                    .position(|e| !e.note.level.sticky())
                    .unwrap_or(0);
                self.history.remove(drop);
            }
            self.history.push_back(Entry {
                at: now,
                title: title.to_string(),
                note: note.clone(),
            });
        }
    }

    /// Advances the clock to `now` and takes down every toast whose life is over.
    /// Returns whether the frame has to be redrawn, and keeps the answer in
    /// [`Self::repaint`]: a toast left, one still counts down its remaining time on
    /// screen, or the history is open and its ages are a second or more behind.
    pub(crate) fn tick(&mut self, now: Instant, history_open: bool) -> bool {
        self.now = Some(now);
        let before = self.toasts.len();
        self.toasts
            .retain(|t| t.until.is_none_or(|until| now < until));
        let ages = history_open
            && self
                .aged
                .is_none_or(|aged| now.saturating_duration_since(aged) >= AGE_STEP);
        if ages {
            self.aged = Some(now);
        } else if !history_open {
            self.aged = None;
        }
        self.repaint = before != self.toasts.len() || self.counting_down() || ages;
        self.repaint
    }

    /// Turns toasts on or off (`[ui] notifications`). Off takes down the toasts on
    /// screen; the history keeps them.
    pub(crate) fn set_toasts_enabled(&mut self, enabled: bool) {
        self.toasts_enabled = enabled;
        if !enabled {
            self.dismiss_all();
        }
    }

    /// Whether a toast on screen counts down its remaining time, which every frame redraws.
    pub(crate) fn counting_down(&self) -> bool {
        self.toasts.iter().any(|t| t.until.is_some())
    }

    /// Takes one toast down. Returns whether it was on screen.
    pub(crate) fn dismiss(&mut self, id: u64) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        before != self.toasts.len()
    }

    /// Takes every toast down; the history keeps them.
    pub(crate) fn dismiss_all(&mut self) {
        self.toasts.clear();
    }
}

/// What one source looked like at a moment, as far as a re-scan summary cares.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceShape {
    /// Still scanning: nothing is known about it yet, so no change can be claimed.
    Unknown,
    /// Its last answer was a failure.
    Unreachable,
    /// Its machine refused the login: an authentication failure, or a held password ssh
    /// refused.
    Locked,
    /// It answered with these sessions.
    Sessions(BTreeSet<String>),
}

impl SourceShape {
    /// Whether the source failed to answer, for either reason.
    fn failed(&self) -> bool {
        matches!(self, SourceShape::Unreachable | SourceShape::Locked)
    }
}

/// The inventory as a re-scan summary compares it: every source and what it answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScanSnapshot {
    sources: BTreeMap<String, SourceShape>,
}

/// How many names a summary line spells out before it counts the rest.
const SUMMARY_NAMES: usize = 4;

impl ScanSnapshot {
    /// The snapshot of `state`'s inventory now. `locked` names the machines whose held
    /// password ssh refused: their cards keep no failure of their own, because the login
    /// owns that diagnosis, so the snapshot cannot read the refusal off the inventory.
    pub(crate) fn of(state: &super::State, locked: &HashSet<String>) -> Self {
        let sources = state
            .groups
            .iter()
            .map(|g| {
                let shape = if state.scanning.contains(&g.source) {
                    SourceShape::Unknown
                } else if locked.contains(crate::session::machine_of(&g.source))
                    || g.failure() == Some(crate::model::FailureKind::Blocked)
                {
                    SourceShape::Locked
                } else if g.err.is_some() {
                    SourceShape::Unreachable
                } else {
                    SourceShape::Sessions(g.sessions.iter().map(|s| s.name.clone()).collect())
                };
                (g.source.clone(), shape)
            })
            .collect();
        ScanSnapshot { sources }
    }

    /// The same snapshot narrowed to the sources `machine` serves, for a re-scan that
    /// asked that machine alone.
    pub(crate) fn only_machine(mut self, machine: &str) -> Self {
        self.sources
            .retain(|source, _| crate::session::machine_of(source) == machine);
        self
    }

    /// One report of what changed between this snapshot and `after`: sources added and
    /// removed, sessions started and ended, and sources that stopped or started
    /// answering. `label` names a source the way its card does. A session is named under
    /// its machine, and under the machine and its mux when the machine serves several. A
    /// re-scan that changed nothing says so, with the counts it found. Every host count
    /// counts machines, not the muxes they serve.
    pub(crate) fn summary(
        &self,
        after: &ScanSnapshot,
        label: impl Fn(&str) -> String,
    ) -> Vec<Note> {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut started = Vec::new();
        let mut ended = Vec::new();
        let mut reachable = Vec::new();
        let mut lost = Vec::new();
        for (source, shape) in &after.sources {
            match (self.sources.get(source), shape) {
                (None, _) => added.push(source.as_str()),
                (Some(SourceShape::Sessions(was)), SourceShape::Sessions(now)) => {
                    let owner = if crate::session::mux_of(source).is_empty() {
                        crate::session::machine_of(source).to_string()
                    } else {
                        label(source)
                    };
                    started.extend(now.difference(was).map(|n| format!("{owner}/{n}")));
                    ended.extend(was.difference(now).map(|n| format!("{owner}/{n}")));
                }
                (Some(SourceShape::Sessions(_)), SourceShape::Unreachable) => lost.push(Note::new(
                    Level::Warning,
                    format!("{} unreachable", label(source)),
                )),
                (Some(SourceShape::Sessions(_)), SourceShape::Locked) => lost.push(Note::new(
                    Level::Warning,
                    format!("{} login needed", label(source)),
                )),
                (Some(was), SourceShape::Sessions(_)) if was.failed() => {
                    reachable.push(label(source))
                }
                _ => {}
            }
        }
        for source in self.sources.keys() {
            if !after.sources.contains_key(source) {
                removed.push(source.as_str());
            }
        }
        let mut lines = Vec::new();
        for (sources, verb) in [(added, "added"), (removed, "removed")] {
            if !sources.is_empty() {
                let names: Vec<String> = sources.iter().map(|s| label(s)).collect();
                lines.push(Note::new(
                    Level::Info,
                    format!(
                        "{} {verb}: {}",
                        counted(machines(sources), "host"),
                        name_list(&names)
                    ),
                ));
            }
        }
        for (names, verb) in [(started, "started"), (ended, "ended")] {
            if !names.is_empty() {
                lines.push(Note::new(
                    Level::Info,
                    format!(
                        "{} {verb}: {}",
                        counted(names.len(), "session"),
                        name_list(&names)
                    ),
                ));
            }
        }
        if !reachable.is_empty() {
            lines.push(Note::new(
                Level::Success,
                format!("{} reachable again", name_list(&reachable)),
            ));
        }
        lines.extend(lost);
        if lines.is_empty() {
            let sessions: usize = after
                .sources
                .values()
                .map(|shape| match shape {
                    SourceShape::Sessions(names) => names.len(),
                    _ => 0,
                })
                .sum();
            lines.push(Note::new(
                Level::Success,
                format!(
                    "no changes · {}, {}",
                    counted(machines(after.sources.keys().map(String::as_str)), "host"),
                    counted(sessions, "session")
                ),
            ));
        }
        lines
    }
}

/// How many machines serve `sources`.
fn machines<'a>(sources: impl IntoIterator<Item = &'a str>) -> usize {
    sources
        .into_iter()
        .map(crate::session::machine_of)
        .collect::<BTreeSet<_>>()
        .len()
}

/// `1 host`, `2 hosts`.
fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The first few names, then how many more there are.
fn name_list(names: &[String]) -> String {
    let shown = names
        .iter()
        .take(SUMMARY_NAMES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SUMMARY_NAMES {
        format!("{shown} … {} more", names.len() - SUMMARY_NAMES)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Vec<Note> {
        vec![Note::new(Level::Success, text)]
    }

    fn err(text: &str) -> Vec<Note> {
        vec![Note::new(Level::Error, text)]
    }

    #[test]
    fn a_success_toast_leaves_after_its_life_and_stays_in_the_history() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(t0, "gpu-02", ok("logged in"));
        assert_eq!(n.toasts.len(), 1);
        assert!(
            n.tick(t0 + Duration::from_secs(1), false),
            "it counts down on screen"
        );
        assert_eq!(n.toasts.len(), 1, "it is still up before its life is over");
        let half = n.toasts[0].remaining(t0 + TOAST_TTL / 2).unwrap();
        assert!((half - 0.5).abs() < 0.01, "half its life is left: {half}");
        assert!(
            n.tick(t0 + TOAST_TTL, false),
            "its leaving redraws the frame"
        );
        assert!(n.toasts.is_empty());
        assert!(
            !n.tick(t0 + TOAST_TTL * 2, false),
            "nothing is left to redraw for"
        );
        assert_eq!(n.history.len(), 1);
        assert_eq!(n.history[0].note.text, "logged in");
    }

    #[test]
    fn an_error_toast_stays_until_it_is_dismissed() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(
            t0,
            "gpu-02",
            vec![
                Note::new(Level::Success, "logged in"),
                Note::new(Level::Error, "public key not registered: denied"),
            ],
        );
        assert_eq!(
            n.toasts[0].remaining(t0),
            None,
            "a failure never counts down"
        );
        assert!(!n.tick(t0 + Duration::from_secs(3600), false));
        assert_eq!(n.toasts.len(), 1, "an hour later it is still up");
        let id = n.toasts[0].id;
        assert!(n.dismiss(id));
        assert!(n.toasts.is_empty());
        assert_eq!(n.history.len(), 2, "both lines stay in the history");
    }

    #[test]
    fn a_warning_toast_stays_too() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(
            t0,
            "re-scan",
            vec![Note::new(Level::Warning, "web-03 unreachable")],
        );
        n.tick(t0 + TOAST_TTL * 4, false);
        assert_eq!(n.toasts.len(), 1);
    }

    #[test]
    fn a_full_stack_drops_its_oldest_timed_toast_first() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(t0, "a", err("first failure"));
        n.toast_at(t0, "b", ok("first success"));
        n.toast_at(t0, "c", err("second failure"));
        n.toast_at(t0, "d", ok("second success"));
        let titles: Vec<&str> = n.toasts.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(
            titles,
            ["a", "c", "d"],
            "the timed toast left, the failures stayed"
        );
        assert_eq!(n.history.len(), 4);
    }

    #[test]
    fn a_new_toast_always_shows_when_every_older_one_waits_to_be_dismissed() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.toast_at(t0, "a", err("first failure"));
        n.toast_at(t0, "b", err("second failure"));
        n.toast_at(t0, "c", err("third failure"));
        n.toast_at(t0, "d", ok("a success"));
        let titles: Vec<&str> = n.toasts.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(
            titles,
            ["b", "c", "d"],
            "the newest stays, the oldest failure leaves the stack"
        );
        assert!(
            n.history.iter().any(|e| e.title == "a"),
            "the history keeps it"
        );
    }

    #[test]
    fn the_open_history_repaints_once_a_second_for_its_ages() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.record_at(t0, "x", &err("failure"));
        assert!(!n.tick(t0, false), "a closed history asks for nothing");
        assert!(n.tick(t0, true), "opening it repaints at once");
        assert!(!n.tick(t0 + Duration::from_millis(500), true));
        assert!(
            n.tick(t0 + Duration::from_millis(1000), true),
            "a second on"
        );
        assert!(n.repaint, "the tick keeps its answer for the loop");
        assert!(!n.tick(t0 + Duration::from_millis(1200), true));
        assert!(!n.repaint);
    }

    #[test]
    fn turning_toasts_off_takes_them_down_and_keeps_the_history() {
        let mut n = Notifications::default();
        n.toast("gpu-02", err("login failed"));
        n.set_toasts_enabled(false);
        assert!(n.toasts.is_empty());
        assert_eq!(n.history.len(), 1);
        n.set_toasts_enabled(true);
        n.toast("gpu-02", ok("logged in"));
        assert_eq!(n.toasts.len(), 1);
    }

    #[test]
    fn a_full_history_drops_info_before_errors() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        n.record_at(t0, "old", &err("oldest failure"));
        for i in 0..HISTORY_CAP - 1 {
            n.record_at(t0, "x", &ok(&format!("routine {i}")));
        }
        assert_eq!(n.history.len(), HISTORY_CAP);
        n.record_at(t0, "new", &err("newest failure"));
        assert_eq!(n.history.len(), HISTORY_CAP, "the history stays bounded");
        assert_eq!(
            n.history[0].note.text, "oldest failure",
            "the oldest failure outlives every routine record"
        );
        assert!(
            !n.history.iter().any(|e| e.note.text == "routine 0"),
            "the oldest routine record went first"
        );
        assert_eq!(n.history.back().unwrap().note.text, "newest failure");
    }

    #[test]
    fn a_history_of_failures_only_drops_its_oldest() {
        let mut n = Notifications::default();
        let t0 = Instant::now();
        for i in 0..HISTORY_CAP + 1 {
            n.record_at(t0, "x", &err(&format!("failure {i}")));
        }
        assert_eq!(n.history.len(), HISTORY_CAP);
        assert_eq!(n.history[0].note.text, "failure 1");
    }

    #[test]
    fn a_background_event_is_recorded_without_a_toast() {
        let mut n = Notifications::default();
        n.record("web-03/tmux", err("unreachable"));
        assert!(n.toasts.is_empty());
        assert_eq!(n.history.len(), 1);
    }

    #[test]
    fn toasts_turned_off_still_reach_the_history() {
        let mut n = Notifications {
            toasts_enabled: false,
            ..Notifications::default()
        };
        n.toast("gpu-02", ok("logged in"));
        assert!(n.toasts.is_empty());
        assert_eq!(n.history.len(), 1);
    }

    fn shape(entries: &[(&str, Option<&[&str]>)]) -> ScanSnapshot {
        ScanSnapshot {
            sources: entries
                .iter()
                .map(|(source, sessions)| {
                    let shape = match sessions {
                        Some(names) => {
                            SourceShape::Sessions(names.iter().map(|n| n.to_string()).collect())
                        }
                        None => SourceShape::Unreachable,
                    };
                    (source.to_string(), shape)
                })
                .collect(),
        }
    }

    fn texts(notes: &[Note]) -> Vec<(Level, String)> {
        notes.iter().map(|n| (n.level, n.text.clone())).collect()
    }

    #[test]
    fn a_rescan_summary_names_every_kind_of_change() {
        let before = shape(&[
            ("gpu-01", Some(&["train", "eval"])),
            ("web-03", Some(&["api"])),
            ("old", Some(&[])),
            ("db", None),
        ]);
        let after = shape(&[
            ("gpu-01", Some(&["train", "serve"])),
            ("web-03", None),
            ("db", Some(&["psql"])),
            ("new", Some(&["x"])),
        ]);
        let notes = before.summary(&after, |s| format!("{s}/tmux"));
        assert_eq!(
            texts(&notes),
            vec![
                (Level::Info, "1 host added: new/tmux".to_string()),
                (Level::Info, "1 host removed: old/tmux".to_string()),
                (Level::Info, "1 session started: gpu-01/serve".to_string()),
                (Level::Info, "1 session ended: gpu-01/eval".to_string()),
                (Level::Success, "db/tmux reachable again".to_string()),
                (Level::Warning, "web-03/tmux unreachable".to_string()),
            ]
        );
    }

    #[test]
    fn a_rescan_that_changed_nothing_says_so_with_its_counts() {
        let before = shape(&[("gpu-01", Some(&["train", "eval"])), ("web-03", None)]);
        let notes = before.summary(&before.clone(), |s| s.to_string());
        assert_eq!(
            texts(&notes),
            vec![(
                Level::Success,
                "no changes · 2 hosts, 2 sessions".to_string()
            )]
        );
    }

    #[test]
    fn a_long_list_of_names_counts_the_rest() {
        let before = shape(&[("h", Some(&[]))]);
        let after = shape(&[("h", Some(&["a", "b", "c", "d", "e", "f"]))]);
        let notes = before.summary(&after, |s| s.to_string());
        assert_eq!(
            notes[0].text,
            "6 sessions started: h/a, h/b, h/c, h/d … 2 more"
        );
    }

    #[test]
    fn host_counts_count_machines_and_a_session_names_its_mux_beside_another() {
        let before = shape(&[
            ("local:tmux", Some(&["a"])),
            ("local:zellij", Some(&[])),
            ("gpu", Some(&["train"])),
        ]);
        let notes = before.summary(&before.clone(), |s| s.to_string());
        assert_eq!(notes[0].text, "no changes · 2 hosts, 2 sessions");

        let after = shape(&[
            ("local:tmux", Some(&["a", "b"])),
            ("local:zellij", Some(&[])),
            ("gpu", Some(&["train", "eval"])),
            ("new:tmux", Some(&[])),
            ("new:zellij", Some(&[])),
        ]);
        let label = |s: &str| s.replace(':', "/");
        let notes = before.summary(&after, label);
        assert_eq!(
            texts(&notes),
            vec![
                (
                    Level::Info,
                    "1 host added: new/tmux, new/zellij".to_string()
                ),
                (
                    Level::Info,
                    "2 sessions started: gpu/eval, local/tmux/b".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_refused_login_reads_as_login_needed_not_as_sessions_ending() {
        let before = shape(&[("gpu", Some(&["train", "eval"]))]);
        let mut after = shape(&[("gpu", Some(&[]))]);
        after.sources.insert("gpu".to_string(), SourceShape::Locked);
        let notes = before.summary(&after, |s| s.to_string());
        assert_eq!(
            texts(&notes),
            vec![(Level::Warning, "gpu login needed".to_string())]
        );
        let back = after.summary(&before, |s| s.to_string());
        assert_eq!(
            texts(&back),
            vec![(Level::Success, "gpu reachable again".to_string())]
        );
    }

    #[test]
    fn a_source_still_scanning_before_the_rescan_claims_no_change() {
        let mut before = shape(&[("h", Some(&[]))]);
        before.sources.insert("h".to_string(), SourceShape::Unknown);
        let after = shape(&[("h", None)]);
        let notes = before.summary(&after, |s| s.to_string());
        assert_eq!(notes[0].level, Level::Success, "{notes:?}");
    }
}
