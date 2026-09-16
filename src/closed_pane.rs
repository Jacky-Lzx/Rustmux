//! One hidden live shell retained for close undo, with no user input routing.
use crate::{
    layout::{Layout, PaneId},
    pane::{MAX_REPLY_DRAIN_BYTES, Pane},
    pane_set::PaneSet,
    window::{WindowId, Windows},
};
use std::io::{self, Read, Write};

pub(crate) struct ClosedPane {
    pub pane: Option<Pane>,
    pub window: WindowId,
    pub name: String,
    pub before: Layout,
    pub after: Option<Layout>,
    pub id: PaneId,
}

impl ClosedPane {
    // Nonblocking bounded maintenance. Hidden PTY readiness wakes the event loop,
    // so output and terminal queries drain without scheduling redraws.
    pub fn service(&mut self) -> io::Result<bool> {
        let (shell, parser, screen, state) = self.pane.as_mut().unwrap().parts_mut();
        if shell.try_wait()?.is_some() {
            return Ok(false);
        }
        let limit = state.reply_read_limit().min(MAX_REPLY_DRAIN_BYTES);
        if limit > 0 {
            let mut bytes = [0; MAX_REPLY_DRAIN_BYTES];
            match shell.read(&mut bytes[..limit]) {
                Ok(0) => return Ok(false),
                Ok(count) => {
                    state.semantic.advance(&bytes[..count]);
                    parser.advance_with_replies(screen, &bytes[..count], &mut |reply| {
                        state.to_shell.extend(reply)
                    })
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if !state.to_shell.is_empty() {
            match shell.write(state.to_shell.as_slices().0) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    state.to_shell.drain(..count);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    pub fn restore(&mut self, windows: &mut Windows<PaneSet<Pane>>) -> io::Result<()> {
        if let Some(window) = windows.get_mut(self.window)
            && let Some(after) = &self.after
        {
            window
                .content_mut()
                .restore_with(&self.before, after, self.id, |rect| {
                    self.pane
                        .as_mut()
                        .unwrap()
                        .prepare_resize(rect.rows, rect.columns)?
                        .commit()?;
                    Ok(self.pane.take().unwrap())
                })?;
            window.content_mut().synchronize_sizes()?;
            windows.select(self.window)?;
        } else {
            if windows.iter().len() >= crate::terminal::MAX_WINDOWS {
                return Err(io::Error::other("window limit reached"));
            }
            let (rows, columns) = windows.active().unwrap().content().layout().dimensions();
            self.pane
                .as_mut()
                .unwrap()
                .prepare_resize(rows, columns)?
                .commit()?;
            windows.create(
                self.name.clone(),
                PaneSet::new(rows, columns, self.pane.take().unwrap())?,
            )?;
        }
        Ok(())
    }
}
