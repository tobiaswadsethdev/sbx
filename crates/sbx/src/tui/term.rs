//! The agent's terminal, inside the TUI.
//!
//! `sbx attach` hands the whole terminal over and takes it back on detach. That
//! is fine for one session and wrong for several: going back and forth means
//! leaving the interface, and comparing two agents means leaving it twice. This
//! module runs the same attach under a pty that `sbx` owns, parses what comes
//! back as a terminal screen, and draws it in the right-hand pane -- so an agent
//! is one keystroke away and stays running whether or not it is on screen.
//!
//! One child process per open session, held for as long as the TUI lives. That
//! is a held `exec --tty` per session, which was the thing worth checking before
//! building any of this: an open attach does *not* queue ahead of ordinary
//! execs, so the status column and the diff pane keep working for a session
//! whose terminal is open. If that ever changes, this is where to look.
//!
//! **Scrolling belongs to the agent, and that took measuring to find out.** The
//! obvious places to put a scrollback are both wrong: a parser on this side sees
//! a full-screen client repainting itself, so nothing ever scrolls off into a
//! buffer, and the sandbox tmux keeps no history either -- Claude Code runs on
//! the *alternate* screen (`#{alternate_on}` is 1, `#{history_size}` is 0), which
//! is exactly the mode that means "no scrollback here". It keeps its own
//! transcript and scrolls it on page-up. So the implementation is to forward the
//! key and stay out of the way; an earlier version of this file entered tmux copy
//! mode instead and replaced a scrollback that worked with an empty one. An agent
//! that does *not* take the alternate screen leaves its history in the sandbox
//! tmux, where `Ctrl-b [` reaches it -- also just forwarded keys.
//!
//! What this deliberately does not do: no mouse and no bracketed paste. Both are
//! about terminal-wide state rather than this pane -- see the notes on each in
//! PLAN.md -- and the routing decision that actually matters, getting *out*, is
//! [`ESCAPE_HINT`].

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

use crate::seed::sh_quote;
use crate::session::{REPO_PATH, Session};

/// How the user gets the keyboard back. F12 rather than a control chord because
/// every chord worth having is already spoken for: `Ctrl-b` is the agent's tmux
/// prefix, `Ctrl-c` and `Esc` interrupt the agent, and `Ctrl-]`-style keys are
/// awkward or impossible on non-US layouts. A function key is layout-independent
/// and nothing inside the sandbox wants it.
pub const ESCAPE_KEY: ratatui::crossterm::event::KeyCode =
    ratatui::crossterm::event::KeyCode::F(12);
/// Shown in the pane title while the terminal has the keyboard, because a way
/// out that has to be looked up is a trap.
pub const ESCAPE_HINT: &str = "F12 to leave";

/// Size to open a pty at before the pane has been measured. Replaced by the
/// real pane size on the first draw; the agent's tmux redraws on resize, so a
/// brief wrong size costs nothing but is worth keeping close to the truth.
const INITIAL: (u16, u16) = (80, 24);

/// The agent's tmux prefix, as its own byte: `Ctrl-b`, which is tmux's default
/// and which the image does not change. Sent with `d` to detach cleanly on the
/// way out; `image.rs` has a test that the image's tmux.conf leaves the prefix
/// alone, because a changed prefix would turn this into two keystrokes typed
/// into whatever the agent happened to be showing.
const TMUX_PREFIX: u8 = 0x02;
/// The size to leave the agent's window at when a terminal is closed.
///
/// tmux resizes a window to its latest client and *keeps* that size after the
/// client goes, so a terminal opened in a narrow pane would leave the agent's
/// window narrow for the rest of its life -- and the status scraper reads that
/// window. Restoring it costs one ioctl and keeps
/// `status::scrape_pane`'s markers off the truncation edge. Must match the
/// image's `default-size`; `image.rs` has a test that it does.
pub const SCRAPE_SIZE: (u16, u16) = (200, 50);

/// How long to give the detach before killing the child.
///
/// Needed because the detach has to travel through the gateway to the sandbox's
/// tmux and back: killing immediately after writing it is a race, and losing
/// that race leaves a client listed as attached for ever -- which pins the
/// agent's window to this pane's size and so decides what every later status
/// scrape reads. Paid once per open terminal at exit, and only when the child
/// does not go on its own.
const DETACH_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

/// Rows of scrollback the *parser* keeps: none, and not for want of trying.
///
/// What arrives over the pty is a full-screen client repainting itself, so lines
/// never scroll off this screen in the way a parser could collect -- a buffer
/// here would stay empty however large it was. Scrolling is the agent's; see the
/// module comment.
const SCROLLBACK: usize = 0;

/// One agent's terminal: the child holding the attach, and the screen it draws.
pub struct Terminal {
    /// Parsed screen state, shared with the reader thread.
    parser: Arc<Mutex<vt100::Parser>>,
    /// The writing half of the pty. Keys typed in the pane land here.
    writer: Box<dyn Write + Send>,
    /// Kept to resize the pty when the pane changes size.
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Last size the pty was told about, so a redraw at an unchanged size does
    /// not turn into a resize the agent has to react to.
    size: (u16, u16),
    /// Set once the child has gone, so the pane can say so rather than showing a
    /// frozen screen that looks live.
    exited: bool,
}

impl Terminal {
    /// Attach to a session's agent under a pty.
    ///
    /// The same script `sbx attach` uses, for the same reasons: `-d` evicts a
    /// client left behind by an earlier crash, and falling through to
    /// `new-session` means this always lands somewhere useful even if the agent
    /// was never started.
    fn open(client: &openshell_client::CliClient, session: &Session) -> Result<Self, String> {
        let script = format!(
            "tmux -f /etc/tmux.conf attach -d -t {tmux} 2>/dev/null \
             || tmux -f /etc/tmux.conf new-session -s {tmux} -c {repo}",
            tmux = sh_quote(&session.tmux),
            repo = sh_quote(REPO_PATH),
        );

        let pty = NativePtySystem::default()
            .openpty(PtySize {
                rows: INITIAL.1,
                cols: INITIAL.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("could not open a pty: {e}"))?;

        // The gateway client is spawned rather than asked for a `Command`, so
        // the argv is built here from the same pieces `interactive_exec` uses.
        let argv = client.interactive_exec_argv(&session.sandbox, &["sh", "-c", &script]);
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        // The child is talking to a pty, so it is entitled to assume a terminal
        // that can draw what the agent draws. Without this the agent falls back
        // to something far plainer than what `sbx attach` shows.
        cmd.env("TERM", "xterm-256color");

        let child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("could not start the attach: {e}"))?;
        // Dropped as soon as the child holds it: keeping the slave open here
        // means the read below never sees EOF when the child exits, and the pane
        // would show a dead terminal as though it were live.
        drop(pty.slave);

        let writer = pty
            .master
            .take_writer()
            .map_err(|e| format!("could not write to the pty: {e}"))?;
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| format!("could not read the pty: {e}"))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            INITIAL.1, INITIAL.0, SCROLLBACK,
        )));

        // One thread per terminal, doing nothing but feeding the parser. The
        // render thread only ever locks the parser to draw, so a chatty agent
        // cannot block the interface -- and a blocking read here costs nothing,
        // unlike polling the pty from the event loop.
        let sink = Arc::clone(&parser);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = sink.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                }
            }
        });

        Ok(Terminal {
            parser,
            writer,
            master: pty.master,
            child,
            size: INITIAL,
            exited: false,
        })
    }

    /// Send bytes to the agent.
    fn send(&mut self, bytes: &[u8]) {
        // A failed write means the child has gone; the exit check on the next
        // draw is what reports it, so there is nothing useful to do here.
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Match the pty to the pane it is drawn in.
    ///
    /// Both halves have to be told: the pty, so the agent's tmux client resizes
    /// and redraws, and the parser, so the screen it keeps is the same shape as
    /// the one being sent.
    fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.size || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// Whether the attach has ended. Checked rather than assumed: an agent whose
    /// tmux was killed, or a gateway that dropped the exec, leaves a screen that
    /// still looks alive.
    fn poll_exited(&mut self) -> bool {
        if !self.exited && matches!(self.child.try_wait(), Ok(Some(_))) {
            self.exited = true;
        }
        self.exited
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Widen the window back before letting go of it, so the next status
        // scrape reads a pane the markers fit in rather than whatever this one
        // happened to be. The resize travels as an ioctl and the detach as
        // bytes, so they take different paths to tmux; the pause is what keeps
        // them in order.
        self.resize(SCRAPE_SIZE.0, SCRAPE_SIZE.1);
        thread::sleep(std::time::Duration::from_millis(50));

        // Detach, wait for it to land, and only then kill. Measured, not
        // assumed: killing straight after the write leaves the sandbox with a
        // client that is still listed as attached, and the agent's window stuck
        // at this pane's size.
        self.send(&[TMUX_PREFIX, b'd']);

        let deadline = std::time::Instant::now() + DETACH_GRACE;
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        // It ignored the detach, or the gateway had already gone. Nothing left
        // to try; a stale client costs a wrong window size, not a lost agent.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every open terminal, keyed by session name.
#[derive(Default)]
pub struct Terminals {
    open: std::collections::HashMap<String, Terminal>,
}

impl Terminals {
    /// Open a session's terminal, or do nothing if it is already open.
    ///
    /// Lazy on purpose: cycling the right pane onto the agent view costs
    /// nothing, and only asking for the keyboard spends a process. Otherwise
    /// walking a list of ten sessions would leave ten attaches running.
    pub fn open(
        &mut self,
        client: &openshell_client::CliClient,
        session: &Session,
    ) -> Result<(), String> {
        if self.open.contains_key(&session.name) {
            return Ok(());
        }
        let term = Terminal::open(client, session)?;
        self.open.insert(session.name.clone(), term);
        Ok(())
    }

    pub fn is_open(&self, name: &str) -> bool {
        self.open.contains_key(name)
    }

    /// The screen to draw, and whether the attach behind it has ended.
    pub fn screen(
        &mut self,
        name: &str,
    ) -> Option<(std::sync::MutexGuard<'_, vt100::Parser>, bool)> {
        let term = self.open.get_mut(name)?;
        let exited = term.poll_exited();
        let parser = term.parser.lock().ok()?;
        Some((parser, exited))
    }

    pub fn send(&mut self, name: &str, bytes: &[u8]) {
        if let Some(term) = self.open.get_mut(name) {
            term.send(bytes);
        }
    }

    pub fn resize(&mut self, name: &str, cols: u16, rows: u16) {
        if let Some(term) = self.open.get_mut(name) {
            term.resize(cols, rows);
        }
    }

    /// Close one terminal, detaching cleanly on the way out. Called when a
    /// session is destroyed, and when the user closes the view by hand.
    pub fn close(&mut self, name: &str) {
        self.open.remove(name);
    }
}

/// Turn a key event into what a terminal would send.
///
/// Pure, and the reason this is testable at all: every key the pane receives
/// goes through here, so "does Ctrl-C reach the agent" is a unit test rather
/// than something to try by hand against a live sandbox.
///
/// `None` means "not for the agent" -- the escape key, and anything with no
/// terminal encoding worth inventing.
pub fn encode_key(key: ratatui::crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes = match key.code {
        // The way out never reaches the agent.
        c if c == ESCAPE_KEY => return None,
        KeyCode::Char(c) => {
            if ctrl {
                // Control characters: `Ctrl-a` is 0x01, and so on. Only the
                // range that has an encoding -- `Ctrl-1` is not a thing.
                let c = c.to_ascii_lowercase();
                match c {
                    'a'..='z' => vec![(c as u8) - b'a' + 1],
                    ' ' | '@' => vec![0],
                    '[' => vec![0x1b],
                    '\\' => vec![0x1c],
                    ']' => vec![0x1d],
                    '^' => vec![0x1e],
                    '_' | '?' => vec![0x1f],
                    _ => return None,
                }
            } else {
                let mut s = c.to_string().into_bytes();
                // Alt is the ESC prefix, which is how every terminal sends it.
                if alt {
                    s.insert(0, 0x1b);
                }
                s
            }
        }
        // `\r`, not `\n`: a terminal sends carriage return, and the agent's
        // readline turns it into a submit. `\n` inserts a newline instead, which
        // is why a prompt typed here would never be sent.
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        // DEL, not BS: what terminals actually send for the key marked
        // backspace, and what the agent's input box expects.
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi(b'A', ctrl, alt),
        KeyCode::Down => csi(b'B', ctrl, alt),
        KeyCode::Right => csi(b'C', ctrl, alt),
        KeyCode::Left => csi(b'D', ctrl, alt),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + (n - 1)],
        KeyCode::F(n @ 5..=12) => {
            // The `~`-terminated encodings, which are not contiguous with F1-F4.
            const CODES: [&[u8]; 8] = [b"15", b"17", b"18", b"19", b"20", b"21", b"23", b"24"];
            let mut out = b"\x1b[".to_vec();
            out.extend_from_slice(CODES[(n - 5) as usize]);
            out.push(b'~');
            out
        }
        _ => return None,
    };
    Some(bytes)
}

/// A CSI arrow, with the modifier parameter terminals use for `Ctrl`/`Alt`.
fn csi(final_byte: u8, ctrl: bool, alt: bool) -> Vec<u8> {
    let modifier = 1 + u8::from(alt) * 2 + u8::from(ctrl) * 4;
    if modifier == 1 {
        return vec![0x1b, b'[', final_byte];
    }
    vec![0x1b, b'[', b'1', b';', b'0' + modifier, final_byte]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn enc(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(KeyEvent::new(code, mods))
    }

    fn plain(code: KeyCode) -> Vec<u8> {
        enc(code, KeyModifiers::NONE).expect("an encoding")
    }

    /// The keys an agent cannot do without. `Ctrl-c` interrupts it, `Esc`
    /// interrupts a turn, and `Enter` submits -- if any of these were swallowed
    /// as a TUI binding the pane would look alive and be useless.
    #[test]
    fn the_keys_that_drive_an_agent_reach_it() {
        assert_eq!(
            enc(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec![3])
        );
        assert_eq!(
            enc(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(vec![4])
        );
        // The agent's tmux prefix, so its own bindings still work in here.
        assert_eq!(
            enc(KeyCode::Char('b'), KeyModifiers::CONTROL),
            Some(vec![2])
        );
        assert_eq!(plain(KeyCode::Esc), vec![0x1b]);
        assert_eq!(plain(KeyCode::Char('q')), b"q".to_vec());
    }

    /// Carriage return, not newline: `\n` inserts a line in the agent's input
    /// box instead of submitting, so a prompt typed in the pane would sit there
    /// looking typed and never be sent.
    #[test]
    fn enter_sends_a_carriage_return() {
        assert_eq!(plain(KeyCode::Enter), vec![b'\r']);
    }

    /// And DEL, not BS, which is what every terminal sends for that key.
    #[test]
    fn backspace_sends_del() {
        assert_eq!(plain(KeyCode::Backspace), vec![0x7f]);
    }

    #[test]
    fn arrows_are_csi_sequences_and_carry_modifiers() {
        assert_eq!(plain(KeyCode::Up), b"\x1b[A".to_vec());
        assert_eq!(plain(KeyCode::Left), b"\x1b[D".to_vec());
        // Ctrl-left, which the agent's input box uses to walk words.
        assert_eq!(
            enc(KeyCode::Left, KeyModifiers::CONTROL),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Right, KeyModifiers::ALT),
            Some(b"\x1b[1;3C".to_vec())
        );
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(
            enc(KeyCode::Char('m'), KeyModifiers::ALT),
            Some(vec![0x1b, b'm'])
        );
    }

    /// The escape key is the one thing that must never reach the agent, or
    /// there would be no way back to the list.
    #[test]
    fn the_escape_key_is_not_forwarded() {
        assert_eq!(enc(ESCAPE_KEY, KeyModifiers::NONE), None);
        assert_eq!(enc(ESCAPE_KEY, KeyModifiers::CONTROL), None);
        // Every other function key still goes through, so nothing else is
        // quietly lost by the same rule.
        assert_eq!(plain(KeyCode::F(1)), vec![0x1b, b'O', b'P']);
        assert_eq!(plain(KeyCode::F(5)), b"\x1b[15~".to_vec());
        assert_eq!(plain(KeyCode::F(11)), b"\x1b[23~".to_vec());
    }

    /// The paging keys are how an agent's own transcript is scrolled, so they
    /// have to go through untouched -- and `Ctrl-b [` has to as well, for an
    /// agent that leaves its history in the sandbox tmux instead.
    #[test]
    fn the_scrolling_keys_are_forwarded_untouched() {
        assert_eq!(plain(KeyCode::PageUp), b"\x1b[5~".to_vec());
        assert_eq!(plain(KeyCode::PageDown), b"\x1b[6~".to_vec());
        assert_eq!(
            enc(KeyCode::Char('b'), KeyModifiers::CONTROL),
            Some(vec![TMUX_PREFIX])
        );
        assert_eq!(plain(KeyCode::Char('[')), b"[".to_vec());
    }

    /// A chord with no terminal encoding must be dropped rather than turned
    /// into a byte that means something else.
    #[test]
    fn keys_with_no_encoding_send_nothing() {
        assert_eq!(enc(KeyCode::Char('1'), KeyModifiers::CONTROL), None);
        assert_eq!(enc(KeyCode::Menu, KeyModifiers::NONE), None);
    }
}
