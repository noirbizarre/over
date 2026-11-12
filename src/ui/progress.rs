//! Command-scoped terminal progress, shared by nested work and concurrent clones.

use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

static ACTIVE: Mutex<Option<Weak<Inner>>> = Mutex::new(None);

#[derive(Clone, Debug)]
pub struct Progress(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    multi: MultiProgress,
    spinner: ProgressBar,
    state: Mutex<State>,
    visible: bool,
}

#[derive(Debug, Default)]
struct State {
    phases: Vec<(u64, String)>,
    next_id: u64,
    pauses: usize,
    closed: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Progress {
    fn new(visible: bool) -> Self {
        let multi = MultiProgress::with_draw_target(target(visible));
        let spinner = multi.add(ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                .expect("static progress template must be valid")
                .tick_chars(super::style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str()),
        );
        if visible {
            // The ticker runs independently of synchronous lint and Git work.
            spinner.enable_steady_tick(Duration::from_millis(80));
        }
        Self(Arc::new(Inner {
            multi,
            spinner,
            state: Mutex::new(State::default()),
            visible,
        }))
    }

    pub fn phase(&self, message: impl Into<String>) -> Phase {
        let message = message.into();
        let mut state = self.0.state.lock().expect("progress state poisoned");
        let id = state.next_id;
        state.next_id += 1;
        state.phases.push((id, message));
        self.refresh(&state);
        Phase {
            progress: self.clone(),
            id,
        }
    }

    fn refresh(&self, state: &State) {
        // Retain participant identity when a nested reconciliation phase changes.
        let message = state
            .phases
            .iter()
            .map(|(_, message)| message.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        self.0
            .spinner
            .set_message(crate::utils::short_paths(&message));
    }

    pub(crate) fn bar(&self, length: u64) -> Bar {
        let _state = self.0.state.lock().expect("progress state poisoned");
        Bar {
            progress: self.clone(),
            bar: self.0.multi.add(ProgressBar::new(length)),
        }
    }

    pub fn pause(&self) -> io::Result<Pause> {
        let mut state = self.0.state.lock().expect("progress state poisoned");
        if state.pauses == 0 && !state.closed {
            // Hiding alone leaves the last frame on screen. Stop all managed writers
            // before clearing so no ticker or concurrent clone can redraw in between.
            self.0.spinner.disable_steady_tick();
            if let Err(error) = self.0.multi.clear() {
                if self.0.visible {
                    self.0.spinner.enable_steady_tick(Duration::from_millis(80));
                }
                return Err(error);
            }
            self.0.multi.set_draw_target(ProgressDrawTarget::hidden());
        }
        state.pauses += 1;
        Ok(Pause(Some(self.clone())))
    }

    fn close(&self) {
        let mut state = self.0.state.lock().expect("progress state poisoned");
        state.closed = true;
        self.0.spinner.finish_and_clear();
        self.0.multi.set_draw_target(ProgressDrawTarget::hidden());
    }
}

fn target(visible: bool) -> ProgressDrawTarget {
    if visible {
        ProgressDrawTarget::stderr()
    } else {
        ProgressDrawTarget::hidden()
    }
}

/// Obtain the CLI's display without making library callers depend on CLI dispatch.
pub fn current() -> Progress {
    active().unwrap_or_default()
}

fn active() -> Option<Progress> {
    ACTIVE
        .lock()
        .expect("progress registry poisoned")
        .as_ref()
        .and_then(Weak::upgrade)
        .map(Progress)
}

pub struct Command {
    progress: Progress,
    _phase: Phase,
}

impl Command {
    pub fn start(message: &str) -> Self {
        let progress = Progress::new(io::stderr().is_terminal());
        let mut active = ACTIVE.lock().expect("progress registry poisoned");
        assert!(
            active.as_ref().and_then(Weak::upgrade).is_none(),
            "a progress command is already active"
        );
        *active = Some(Arc::downgrade(&progress.0));
        drop(active);
        Self {
            _phase: progress.phase(message),
            progress,
        }
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        self.progress.close();
        let mut active = ACTIVE.lock().expect("progress registry poisoned");
        if active
            .as_ref()
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.progress.0)))
        {
            *active = None;
        }
    }
}

#[must_use = "the phase ends when its guard is dropped"]
pub struct Phase {
    progress: Progress,
    id: u64,
}

impl Phase {
    pub fn set_message(&self, message: impl Into<String>) {
        let message = message.into();
        let mut state = self
            .progress
            .0
            .state
            .lock()
            .expect("progress state poisoned");
        if let Some((_, text)) = state.phases.iter_mut().find(|(id, _)| *id == self.id) {
            *text = message;
        }
        self.progress.refresh(&state);
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let mut state = self
            .progress
            .0
            .state
            .lock()
            .expect("progress state poisoned");
        state.phases.retain(|(id, _)| *id != self.id);
        self.progress.refresh(&state);
    }
}

#[must_use = "progress resumes when the pause guard is dropped"]
pub struct Pause(Option<Progress>);

/// Pause tokens can cross awaits: they never retain a mutex or indicatif lock.
pub fn pause() -> io::Result<Pause> {
    active().map_or(Ok(Pause(None)), |progress| progress.pause())
}

/// The closure must only write already-formatted output, never re-enter progress.
pub(super) fn with_output<R>(write: impl FnOnce() -> R) -> R {
    match active() {
        Some(progress) => progress.0.multi.suspend(write),
        None => write(),
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        if let Some(progress) = &self.0 {
            let mut state = progress.0.state.lock().expect("progress state poisoned");
            state.pauses -= 1;
            if state.pauses == 0 && !state.closed {
                progress.0.multi.set_draw_target(target(progress.0.visible));
                if progress.0.visible {
                    progress
                        .0
                        .spinner
                        .enable_steady_tick(Duration::from_millis(80));
                }
            }
        }
    }
}

pub(crate) struct Bar {
    progress: Progress,
    bar: ProgressBar,
}

impl Bar {
    /// Serialize draws with pause transitions; callbacks must not log or await.
    pub(crate) fn update<R>(&self, update: impl FnOnce(&ProgressBar) -> R) -> R {
        let _state = self
            .progress
            .0
            .state
            .lock()
            .expect("progress state poisoned");
        update(&self.bar)
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        let _state = self
            .progress
            .0
            .state
            .lock()
            .expect("progress state poisoned");
        self.bar.finish_and_clear();
        self.progress.0.multi.remove(&self.bar);
    }
}

/// Buffer a tracing event before touching progress: field formatting may itself log.
#[derive(Default)]
pub(super) struct TraceWriter(Vec<u8>);

impl Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.0.is_empty() {
            with_output(|| -> io::Result<()> {
                let mut stderr = io::stderr().lock();
                let message = crate::utils::short_paths(&String::from_utf8_lossy(&self.0));
                stderr.write_all(message.as_bytes())?;
                stderr.flush()
            })?;
            self.0.clear();
        }
        Ok(())
    }
}

impl Drop for TraceWriter {
    fn drop(&mut self) {
        // Write cannot report drop failures; explicit flush still propagates errors.
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use indicatif::ProgressDrawTarget;

    use super::Progress;

    #[derive(Clone, Debug, Default)]
    struct RecordingTerm(Arc<Mutex<String>>);

    impl indicatif::TermLike for RecordingTerm {
        fn width(&self) -> u16 {
            100
        }
        fn move_cursor_up(&self, _: usize) -> io::Result<()> {
            Ok(())
        }
        fn move_cursor_down(&self, _: usize) -> io::Result<()> {
            Ok(())
        }
        fn move_cursor_right(&self, _: usize) -> io::Result<()> {
            Ok(())
        }
        fn move_cursor_left(&self, _: usize) -> io::Result<()> {
            Ok(())
        }
        fn write_line(&self, text: &str) -> io::Result<()> {
            self.write_str(text)
        }
        fn write_str(&self, text: &str) -> io::Result<()> {
            self.0.lock().unwrap().push_str(text);
            Ok(())
        }
        fn clear_line(&self) -> io::Result<()> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
        fn flush(&self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn phase_messages_abbreviate_home_paths_without_changing_stored_messages() {
        let home = dirs::home_dir().unwrap();
        let progress = Progress::default();
        let message = format!("Applying {}", home.join("overlay").display());
        let phase = progress.phase(message.clone());
        assert_eq!(
            progress.0.spinner.message(),
            format!("Applying ~{}overlay", std::path::MAIN_SEPARATOR)
        );
        assert!(
            progress
                .0
                .state
                .lock()
                .unwrap()
                .phases
                .iter()
                .any(|(_, value)| value == &message)
        );
        phase.set_message(format!("Checking {}", home.display()));
        assert_eq!(progress.0.spinner.message(), "Checking ~");
    }

    #[test]
    fn pausing_clears_the_last_frame_before_hidden_updates_can_run() {
        let progress = Progress::default();
        let terminal = RecordingTerm::default();
        progress
            .0
            .multi
            .set_draw_target(ProgressDrawTarget::term_like(Box::new(terminal.clone())));
        let phase = progress.phase("working");
        progress.0.spinner.tick();
        assert!(terminal.0.lock().unwrap().contains("working"));
        let pause = progress.pause().unwrap();
        assert_eq!(*terminal.0.lock().unwrap(), "");
        phase.set_message("hidden update");
        progress.0.spinner.tick();
        assert_eq!(*terminal.0.lock().unwrap(), "");
        drop(pause);
        assert!(progress.0.multi.is_hidden());
    }

    #[test]
    fn dropping_an_outer_phase_does_not_restore_a_stale_message() {
        let progress = Progress::default();
        let outer = progress.phase("overlay");
        let inner = progress.phase("files");
        assert_eq!(progress.0.spinner.message(), "overlay · files");
        drop(outer);
        assert_eq!(progress.0.spinner.message(), "files");
        drop(inner);
        assert_eq!(progress.0.spinner.message(), "");
    }

    #[test]
    fn nested_pauses_preserve_a_hidden_target_and_close_prevents_restoration() {
        let progress = Progress::default();
        let first = progress.pause().unwrap();
        let second = progress.pause().unwrap();
        drop(first);
        assert_eq!(progress.0.state.lock().unwrap().pauses, 1);
        assert!(progress.0.multi.is_hidden());
        progress.close();
        drop(second);
        assert_eq!(progress.0.state.lock().unwrap().pauses, 0);
        assert!(progress.0.multi.is_hidden());
    }

    #[test]
    fn an_error_releases_nested_progress_scopes() {
        let progress = Progress::default();
        let _parent = progress.phase("parent");
        let run = || -> anyhow::Result<()> {
            let _phase = progress.phase("child");
            let _pause = progress.pause()?;
            anyhow::bail!("failure")
        };
        assert!(run().is_err());
        assert_eq!(progress.0.spinner.message(), "parent");
        assert_eq!(progress.0.state.lock().unwrap().pauses, 0);
    }
}
