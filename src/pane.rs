//! Per-pane process and terminal state. Polling and rendering belong to the caller.

#[cfg(test)]
use crate::graphics_snapshot::SourceImagePlacement;
use crate::{
    graphics::{GraphicsEvent, GraphicsFramer},
    graphics_decode::DecodedImage,
    graphics_snapshot::{
        ImageBand, ImagePlanes, SnapshotError, SourceImageProgress, compose_pane_band,
        compose_pane_planes, compose_pane_snapshot, source_image_pane_band_progress,
    },
    graphics_store::{CellAnchor, CellPixelSize, ImageStore, PixelSize},
    graphics_transfer::{AssembledDirectTransfer, DirectTransferAssembler},
    parser::Parser,
    pty::PtyShell,
    screen::Screen,
    semantic::{PromptEvent, SemanticOutput},
};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::Winsize;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    io,
    process::ExitStatus,
    time::{Duration, Instant},
};
use tempfile::{Builder, NamedTempFile};

pub(crate) const INPUT_LIMIT: usize = 64 * 1024;

pub(crate) const MAX_CELLS: usize = 64 * 1024;
pub(crate) const MAX_REPLY_DRAIN_BYTES: usize = 8192;
/// Owns one nonblocking PTY, incremental parser and screen. Moving a pane between
/// windows does not restart its process or reset its parsing state. Dropping it
/// uses PtyShell's close/kill/reap behavior for the direct child.
#[derive(Debug)]
pub struct Pane {
    shell: PtyShell,
    parser: Parser,
    graphics_framer: GraphicsFramer,
    graphics_transfer: DirectTransferAssembler,
    image_store: ImageStore,
    screen: Screen,
    io: PaneIo,
    command_bell_after: Option<Duration>,
    _temporary_file: Option<TemporaryFile>,
}

enum GraphicsSink<'a> {
    Drop,
    Callback(&'a mut dyn FnMut(AssembledDirectTransfer)),
    Store {
        cell_pixels: Option<CellPixelSize>,
        answer_graphics: bool,
        validate_images: bool,
    },
}

#[derive(Debug)]
struct TemporaryFile(NamedTempFile);

impl TemporaryFile {
    fn snapshot(text: &str) -> io::Result<Self> {
        let mut file = Builder::new()
            .prefix("rustmux-snapshot-")
            .suffix(".txt")
            .tempfile()?;
        file.write_all(text.as_bytes())?;
        Ok(Self(file))
    }
}

/// Prepared screen storage with exclusive access to its originating pane.
/// Dropping without commit leaves the pane and PTY untouched. Holding this borrow
/// prevents output from making the prepared screen stale before it is committed.
#[must_use = "commit the prepared resize or drop it to cancel"]
pub struct PreparedPaneResize<'a> {
    pane: &'a mut Pane,
    screen: Option<Screen>,
    prompt_start: Option<(usize, usize)>,
    rows: u16,
    columns: u16,
    cell_pixels: Option<(u16, u16)>,
}

impl PreparedPaneResize<'_> {
    /// Update the live PTY first, then install the already prepared screen.
    /// A PTY error leaves this pane's screen and I/O metadata unchanged.
    /// Already observed EOF or exit needs only the model update.
    pub fn commit(self) -> io::Result<()> {
        let Self {
            pane,
            screen,
            prompt_start,
            rows,
            columns,
            cell_pixels,
        } = self;
        if pane.io.status.is_none() && !pane.io.eof {
            pane.shell
                .resize_with_cell_pixels(rows, columns, cell_pixels)?;
        }
        if let Some(screen) = screen {
            pane.screen = screen;
        }
        pane.io.prompt_start = prompt_start;
        pane.screen.set_synchronized_output(false);
        pane.io.synchronized_since = None;
        pane.io.dirty = true;
        Ok(())
    }
}

/// State that must follow the child when focus changes. The physical terminal's
/// output queue, renderer cache and frame cadence remain shared by the event loop.
#[derive(Debug)]
pub(crate) struct PaneIo {
    pub to_shell: VecDeque<u8>,
    pub dirty: bool,
    pub synchronized_since: Option<Instant>,
    pub eof: bool,
    pub eof_at: Option<Instant>,
    pub status: Option<ExitStatus>,
    pub semantic: SemanticOutput,
    pub prompt_start: Option<(usize, usize)>,
    pub bell_pending: bool,
    command_bell_pending: bool,
}

impl Default for PaneIo {
    fn default() -> Self {
        Self {
            to_shell: VecDeque::new(),
            dirty: true,
            synchronized_since: None,
            eof: false,
            eof_at: None,
            status: None,
            semantic: SemanticOutput::default(),
            prompt_start: None,
            bell_pending: false,
            command_bell_pending: false,
        }
    }
}

impl PaneIo {
    pub fn accepts_input(&self) -> bool {
        !self.eof && self.status.is_none() && self.to_shell.len() < INPUT_LIMIT
    }

    pub fn reply_read_limit(&self) -> usize {
        if self.eof {
            0
        } else if self.status.is_some() {
            // No live child to receive replies; drain its final output.
            MAX_REPLY_DRAIN_BYTES
        } else {
            (INPUT_LIMIT - self.to_shell.len()) / crate::parser::MAX_REPLY_BYTES
        }
    }
}

impl Pane {
    /// Construct the screen before starting a process, then make the master
    /// nonblocking. Startup failures leave no live child behind. Follow
    /// PtyShell::spawn's single-threaded process-spawning requirement.
    pub fn spawn(shell: impl AsRef<OsStr>, rows: u16, columns: u16) -> io::Result<Self> {
        Self::spawn_in(
            shell,
            None,
            rows,
            columns,
            crate::config::Notifications::default(),
            crate::config::DEFAULT_SCROLLBACK_LINES,
        )
    }

    pub(crate) fn spawn_in(
        shell: impl AsRef<OsStr>,
        directory: Option<&Path>,
        rows: u16,
        columns: u16,
        notifications: crate::config::Notifications,
        scrollback_lines: usize,
    ) -> io::Result<Self> {
        if usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane exceeds cell limit",
            ));
        }
        let screen = Screen::new_with_scrollback_limit(
            usize::from(rows),
            usize::from(columns),
            scrollback_lines,
        )?;
        let directory = directory.filter(|path| path.is_dir());
        let shell = PtyShell::spawn_in(shell, directory, rows, columns)?;
        let master = shell.master_fd().expect("new PTY is open");
        let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
        fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        let mut io = PaneIo::default();
        if let Some(directory) = directory {
            io.semantic.set_current_directory(directory.to_owned());
        }
        Ok(Self {
            shell,
            parser: Parser::new(),
            graphics_framer: GraphicsFramer::new(),
            graphics_transfer: DirectTransferAssembler::new(),
            image_store: ImageStore::new(),
            screen,
            io,
            command_bell_after: notifications.command_bell_after(),
            _temporary_file: None,
        })
    }

    /// Start an editor in its own pane with a private, automatically removed snapshot file.
    pub(crate) fn spawn_editor(
        text: &str,
        rows: u16,
        columns: u16,
        scrollback_lines: usize,
    ) -> io::Result<Self> {
        if usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane exceeds cell limit",
            ));
        }
        let screen = Screen::new_with_scrollback_limit(
            usize::from(rows),
            usize::from(columns),
            scrollback_lines,
        )?;
        let temporary_file = TemporaryFile::snapshot(text)?;
        let shell = PtyShell::spawn_editor(temporary_file.0.path().as_os_str(), rows, columns)?;
        let master = shell.master_fd().expect("new PTY is open");
        let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
        fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self {
            shell,
            parser: Parser::new(),
            graphics_framer: GraphicsFramer::new(),
            graphics_transfer: DirectTransferAssembler::new(),
            image_store: ImageStore::new(),
            screen,
            io: PaneIo::default(),
            command_bell_after: None,
            _temporary_file: Some(temporary_file),
        })
    }

    /// Keep the shell but discard user input intended for the stopped foreground job.
    pub(crate) fn stop_for_hide(&mut self) -> io::Result<()> {
        let stopped = self.shell.stop_foreground()?;
        self.io.semantic.cancel_current();
        if stopped {
            // A killed full-screen job cannot restore these modes itself.
            self.parser = Parser::new();
            self.graphics_framer = GraphicsFramer::new();
            self.graphics_transfer.reset();
            self.image_store.clear();
            self.screen.leave_alternate();
            self.screen.soft_reset();
            self.screen
                .set_mouse_tracking(crate::screen::MouseTracking::Off);
            self.screen.set_sgr_mouse(false);
            self.screen.set_focus_reporting(false);
            self.screen.set_bracketed_paste(false);
        }
        self.io.to_shell.clear();
        Ok(())
    }

    pub fn shell(&self) -> &PtyShell {
        &self.shell
    }

    /// Raw PTY access for readiness-driven reads/writes and lifecycle handling.
    /// Reads must be passed to process_output; readiness and queue limits are
    /// caller responsibilities. Direct resize must also update the screen.
    pub fn shell_mut(&mut self) -> &mut PtyShell {
        &mut self.shell
    }

    /// Validate dimensions and prepare resized grids before issuing any PTY ioctl.
    /// Same-size requests avoid copying screen cells. A commit still releases any
    /// synchronized-output hold and schedules redraw, as a terminal resize does.
    pub fn prepare_resize(
        &mut self,
        rows: u16,
        columns: u16,
    ) -> io::Result<PreparedPaneResize<'_>> {
        let cell_pixels = self.shell.cell_pixels();
        self.prepare_resize_inner(rows, columns, cell_pixels)
    }

    pub(crate) fn prepare_resize_with_cell_pixels(
        &mut self,
        rows: u16,
        columns: u16,
        cell_pixels: Option<CellPixelSize>,
    ) -> io::Result<PreparedPaneResize<'_>> {
        self.prepare_resize_inner(
            rows,
            columns,
            cell_pixels.map(|cell| (cell.width(), cell.height())),
        )
    }

    pub(crate) fn sync_pty_cell_pixels(
        &mut self,
        cell_pixels: Option<CellPixelSize>,
    ) -> io::Result<()> {
        self.shell
            .sync_cell_pixels(cell_pixels.map(|cell| (cell.width(), cell.height())))
    }

    fn prepare_resize_inner(
        &mut self,
        rows: u16,
        columns: u16,
        cell_pixels: Option<(u16, u16)>,
    ) -> io::Result<PreparedPaneResize<'_>> {
        if rows == 0 || columns == 0 || usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid pane dimensions",
            ));
        }
        let mut prompt_start = self.io.prompt_start;
        let screen = if self.screen.dimensions() == (usize::from(rows), usize::from(columns)) {
            None
        } else {
            let mut screen = self.screen.clone();
            if columns != self.screen.dimensions().1 as u16 {
                if let Some(start) = prompt_start {
                    prompt_start = Some(screen.resize_preserving_tail(
                        usize::from(rows),
                        usize::from(columns),
                        start,
                    )?);
                } else {
                    screen.resize(usize::from(rows), usize::from(columns))?;
                }
            } else {
                let cursor_offset = prompt_start
                    .map(|start| (self.screen.cursor().0.saturating_sub(start.0), start.1));
                screen.resize(usize::from(rows), usize::from(columns))?;
                if let Some((offset, column)) = cursor_offset {
                    prompt_start = Some((
                        screen.cursor().0.saturating_sub(offset),
                        column.min(usize::from(columns) - 1),
                    ));
                }
            }
            Some(screen)
        };
        Ok(PreparedPaneResize {
            pane: self,
            screen,
            prompt_start,
            rows,
            columns,
            cell_pixels,
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) fn last_command_output(&self) -> Option<String> {
        self.io.semantic.last_output()
    }

    pub(crate) fn terminal_title(&self) -> &str {
        self.io
            .semantic
            .title()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("shell")
    }

    pub(crate) fn command_submitted(&mut self) {
        self.io.semantic.command_submitted();
        self.io.prompt_start = None;
    }

    pub(crate) fn inherited_directory(&self) -> Option<PathBuf> {
        self.shell
            .inherited_directory(self.io.semantic.current_directory())
    }

    /// Consume child output and route terminal replies back to this same child.
    /// The caller must reserve reply capacity before reading (MAX_REPLY_BYTES).
    pub fn process_output(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        self.process_output_inner(bytes, reply, GraphicsSink::Drop);
    }

    /// Runtime path: retain graphics commands and answer supported direct-data
    /// queries and uploads only when this attachment can display their images.
    pub(crate) fn process_output_for_runtime(
        &mut self,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
        cell_pixels: Option<CellPixelSize>,
        answer_graphics: bool,
    ) {
        self.process_output_inner(
            bytes,
            reply,
            GraphicsSink::Store {
                cell_pixels,
                answer_graphics,
                validate_images: true,
            },
        );
    }

    /// Opt in to receiving complete direct-data transfers through a callback,
    /// separately from the runtime's bounded image store. This opt-in path
    /// does not send Kitty graphics capability replies.
    pub fn process_output_with_graphics(
        &mut self,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
        graphics: &mut impl FnMut(AssembledDirectTransfer),
    ) {
        self.process_output_inner(bytes, reply, GraphicsSink::Callback(graphics));
    }

    /// Opt in to bounded, pane-local image data and placement references.
    /// Cell anchors, screen clears, vertical row shifts and explicit-cell
    /// cursor motion are modeled; pixel rendering and replies are not.
    pub fn process_output_with_image_store(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        self.process_output_inner(
            bytes,
            reply,
            GraphicsSink::Store {
                cell_pixels: None,
                answer_graphics: false,
                validate_images: false,
            },
        );
    }

    /// Opt in to image storage with a caller-verified physical cell size.
    /// Missing placement extents can then be derived from decoded image pixels.
    pub fn process_output_with_image_store_sized(
        &mut self,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
        cell_pixels: CellPixelSize,
    ) {
        self.process_output_inner(
            bytes,
            reply,
            GraphicsSink::Store {
                cell_pixels: Some(cell_pixels),
                answer_graphics: false,
                validate_images: false,
            },
        );
    }

    /// Opt in to image storage using an outer terminal's reported window size.
    /// Inexact or absent pixel dimensions leave placement extents unresolved.
    pub fn process_output_with_image_store_for_terminal(
        &mut self,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
        terminal: Winsize,
    ) {
        let cell_pixels = CellPixelSize::from_terminal_size(
            terminal.ws_row,
            terminal.ws_col,
            terminal.ws_xpixel,
            terminal.ws_ypixel,
        );
        self.process_output_inner(
            bytes,
            reply,
            GraphicsSink::Store {
                cell_pixels,
                answer_graphics: false,
                validate_images: false,
            },
        );
    }

    pub fn image_store(&self) -> &ImageStore {
        &self.image_store
    }

    pub fn image_store_mut(&mut self) -> &mut ImageStore {
        &mut self.image_store
    }

    /// Only placeholder cells affect virtual image overlays. Ordinary text
    /// changes do not require resending an unchanged image plane.
    pub(crate) fn virtual_placeholder_revision(&self) -> u64 {
        if !self
            .image_store
            .placements()
            .any(|placement| placement.virtual_layout.is_some())
        {
            return 0;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.screen.dimensions().hash(&mut hash);
        self.screen.is_alternate().hash(&mut hash);
        for row in 0..self.screen.dimensions().0 {
            for (column, cell) in self.screen.row(row).unwrap().iter().enumerate() {
                if cell.character == crate::graphics_placeholder::PLACEHOLDER_CHAR {
                    (row, column).hash(&mut hash);
                    cell.combining.hash(&mut hash);
                    cell.style.foreground.hash(&mut hash);
                    cell.style.underline_color.hash(&mut hash);
                }
            }
        }
        hash.finish()
    }

    /// Produce an image-only RGBA snapshot for the current screen when the
    /// caller knows the physical cell size. Text and backgrounds are omitted;
    /// the ordinary runtime uses the separate stacking-band path below.
    pub fn compose_image_snapshot(
        &self,
        cell_pixels: CellPixelSize,
    ) -> Result<DecodedImage, SnapshotError> {
        let viewport = self.image_viewport(cell_pixels)?;
        compose_pane_snapshot(&self.image_store, &self.screen, viewport, cell_pixels)
    }

    /// Return only populated Kitty image stacking bands. This is still an
    /// image-only result; it does not paint glyphs or cell backgrounds.
    pub fn compose_image_planes(
        &self,
        cell_pixels: CellPixelSize,
    ) -> Result<ImagePlanes, SnapshotError> {
        let viewport = self.image_viewport(cell_pixels)?;
        compose_pane_planes(&self.image_store, &self.screen, viewport, cell_pixels)
    }

    /// Compose one stacking band without decoding or allocating the others.
    pub fn compose_image_band(
        &self,
        cell_pixels: CellPixelSize,
        band: ImageBand,
    ) -> Result<Option<DecodedImage>, SnapshotError> {
        let viewport = self.image_viewport(cell_pixels)?;
        compose_pane_band(&self.image_store, &self.screen, viewport, cell_pixels, band)
    }

    #[cfg(test)]
    pub(crate) fn source_image_band(
        &self,
        cell_pixels: CellPixelSize,
        band: ImageBand,
    ) -> Option<SourceImagePlacement<'_>> {
        let viewport = self.image_viewport(cell_pixels).ok()?;
        crate::graphics_snapshot::source_image_pane_band(
            &self.image_store,
            &self.screen,
            viewport,
            cell_pixels,
            band,
        )
    }

    pub(crate) fn source_image_band_progress(
        &self,
        cell_pixels: CellPixelSize,
        band: ImageBand,
    ) -> Option<SourceImageProgress<'_>> {
        let viewport = self.image_viewport(cell_pixels).ok()?;
        source_image_pane_band_progress(
            &self.image_store,
            &self.screen,
            viewport,
            cell_pixels,
            band,
        )
    }

    /// Compose the z >= 0 band without decoding or allocating the two
    /// negative-z bands.
    pub fn compose_above_text_image(
        &self,
        cell_pixels: CellPixelSize,
    ) -> Result<Option<DecodedImage>, SnapshotError> {
        self.compose_image_band(cell_pixels, ImageBand::AboveText)
    }

    fn image_viewport(&self, cell_pixels: CellPixelSize) -> Result<PixelSize, SnapshotError> {
        let (rows, columns) = self.screen.dimensions();
        Ok(PixelSize {
            width: u32::try_from(columns)
                .ok()
                .and_then(|columns| columns.checked_mul(u32::from(cell_pixels.width())))
                .ok_or(SnapshotError::InvalidViewport)?,
            height: u32::try_from(rows)
                .ok()
                .and_then(|rows| rows.checked_mul(u32::from(cell_pixels.height())))
                .ok_or(SnapshotError::InvalidViewport)?,
        })
    }

    fn process_output_inner(
        &mut self,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
        mut graphics: GraphicsSink<'_>,
    ) {
        self.io.dirty = true;
        let cell_pixels = match &graphics {
            GraphicsSink::Store { cell_pixels, .. } => *cell_pixels,
            _ => None,
        };
        self.parser
            .set_cell_pixels(cell_pixels.map(|cell| (cell.width(), cell.height())));
        if matches!(graphics, GraphicsSink::Drop) {
            self.graphics_transfer.reset();
        }
        for event in self.graphics_framer.advance(bytes) {
            match event {
                GraphicsEvent::Terminal(bytes) => self.process_terminal_output(&bytes, reply),
                GraphicsEvent::Command(command) => match &mut graphics {
                    GraphicsSink::Drop => {}
                    GraphicsSink::Callback(handler) => {
                        if let Some(transfer) = self.graphics_transfer.accept(&command) {
                            handler(transfer);
                        }
                    }
                    GraphicsSink::Store {
                        cell_pixels,
                        answer_graphics,
                        validate_images,
                    } => {
                        let (row, column) = self.screen.cursor();
                        let anchor = CellAnchor {
                            row,
                            column,
                            alternate: self.screen.is_alternate(),
                        };
                        let transfer =
                            self.graphics_transfer.accept(&command).map(Ok).or_else(|| {
                                crate::graphics_transfer::shared_memory_transfer(&command)
                                    .or_else(|| crate::graphics_transfer::file_transfer(&command))
                            });
                        let transfer = match transfer {
                            Some(Err(())) => {
                                if let Some(response) =
                                    crate::graphics_reply::medium_read_error_reply(
                                        &command,
                                        *answer_graphics,
                                    )
                                {
                                    reply(&response);
                                }
                                continue;
                            }
                            Some(Ok(transfer)) => Some(transfer),
                            None => None,
                        };
                        let placed = if let Some(transfer) = transfer {
                            if transfer.control(b'a') == Some(b"q".as_slice()) {
                                if let Some(response) = crate::graphics_reply::direct_query_reply(
                                    transfer,
                                    *answer_graphics,
                                ) {
                                    reply(&response);
                                }
                                continue;
                            }
                            let transfer_reply = crate::graphics_reply::TransferReply::for_transfer(
                                &transfer,
                                *answer_graphics,
                            );
                            let stored = self.image_store.insert_for_pane(
                                transfer,
                                anchor,
                                *cell_pixels,
                                *validate_images,
                            );
                            if let Some(response) = transfer_reply.and_then(|transfer| {
                                transfer.response(
                                    stored.as_ref().map(|(id, _)| *id).map_err(|error| *error),
                                )
                            }) {
                                reply(&response);
                            }
                            stored.map(|(_, geometry)| geometry)
                        } else {
                            if let Some(response) = crate::graphics_reply::unsupported_medium_reply(
                                &command,
                                *answer_graphics,
                            ) {
                                reply(&response);
                                continue;
                            }
                            let placement_reply =
                                crate::graphics_reply::PlacementReply::for_command(
                                    &command,
                                    *answer_graphics,
                                );
                            let placed = self.image_store.accept_control_for_pane(
                                &command,
                                anchor,
                                *cell_pixels,
                                self.screen.dimensions(),
                            );
                            if let Some(response) = placement_reply.and_then(|placement| {
                                placement.response(
                                    placed.as_ref().err().copied(),
                                    placed.as_ref().ok().and_then(|(id, _)| *id),
                                )
                            }) {
                                reply(&response);
                            }
                            placed.map(|(_, geometry)| geometry)
                        };
                        if let Ok(Some(geometry)) = placed
                            && !geometry.cursor_stays
                            && let (Some(columns), Some(rows)) = (geometry.columns, geometry.rows)
                        {
                            self.screen.move_to(
                                row.saturating_add(rows as usize),
                                column.saturating_add(columns as usize),
                            );
                        }
                    }
                },
            }
        }
        let completed_commands = self.io.semantic.take_completed_commands();
        if self.command_bell_after.is_some_and(|threshold| {
            completed_commands
                .into_iter()
                .any(|duration| duration >= threshold)
        }) {
            self.io.bell_pending = true;
            self.io.command_bell_pending = true;
        }
    }

    fn process_terminal_output(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let mut events = Vec::new();
        self.io
            .semantic
            .advance_with_prompt_events(bytes, &mut |offset, event| events.push((offset, event)));
        let mut start = 0;
        for (end, event) in events {
            self.process_output_segment(&bytes[start..end], reply);
            self.io.prompt_start = match event {
                PromptEvent::Start => Some(self.screen.cursor()),
                PromptEvent::End => None,
            };
            start = end;
        }
        self.process_output_segment(&bytes[start..], reply);
    }

    pub(crate) fn take_command_bell(&mut self) -> bool {
        std::mem::take(&mut self.io.command_bell_pending)
    }

    fn process_output_segment(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let before = self.screen.primary_scroll_count();
        self.parser
            .advance_with_replies(&mut self.screen, bytes, reply);
        let (scroll_events, scroll_overflowed) = self.screen.take_scroll_events();
        if scroll_overflowed {
            // Preserve image data, but do not retain positions after losing
            // physical row-shift events from an unusually large input batch.
            self.image_store.clear_screen_placements(false);
            self.image_store.clear_screen_placements(true);
        } else {
            let history_len = self.screen.history_len();
            for event in scroll_events {
                self.image_store.scroll_placements(event, history_len);
            }
        }
        let graphics_clears = self.parser.take_graphics_clears();
        for (alternate, clear) in graphics_clears.into_iter().enumerate() {
            if clear {
                self.image_store.clear_screen_placements(alternate != 0);
            }
        }
        self.io.bell_pending |= self.parser.take_bell();
        let scrolled = self.screen.primary_scroll_count().saturating_sub(before);
        if let Some((row, column)) = self.io.prompt_start {
            self.io.prompt_start = Some((row.saturating_sub(scrolled as usize), column));
        }
    }

    /// Flush an incomplete UTF-8 sequence when the caller observes PTY EOF.
    pub fn finish_output(&mut self) {
        for event in self.graphics_framer.finish() {
            if let GraphicsEvent::Terminal(bytes) = event {
                self.process_terminal_output(&bytes, &mut |_| {});
            }
        }
        self.graphics_transfer.reset();
        self.image_store.clear();
        self.parser.finish(&mut self.screen);
        self.io.dirty = true;
        self.io.eof = true;
        self.io.eof_at.get_or_insert_with(Instant::now);
    }

    pub(crate) fn io(&self) -> &PaneIo {
        &self.io
    }

    // Borrow disjoint pane state for readiness-driven event-loop operations.
    pub(crate) fn parts_mut(&mut self) -> (&mut PtyShell, &mut Parser, &mut Screen, &mut PaneIo) {
        (
            &mut self.shell,
            &mut self.parser,
            &mut self.screen,
            &mut self.io,
        )
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;
    use crate::{parser::MAX_REPLY_BYTES, window::Windows};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use nix::libc;
    use std::ffi::CString;
    use std::{os::unix::process::ExitStatusExt, time::Duration};

    fn pane_test_shm(suffix: &str, data: &[u8]) -> CString {
        let name = CString::new(format!("/rustmux-pane-{}-{suffix}", std::process::id())).unwrap();
        // SAFETY: name is a NUL-terminated POSIX SHM name.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        assert!(fd >= 0, "shm_open: {}", std::io::Error::last_os_error());
        // SAFETY: the descriptor is valid and data fits off_t.
        assert_eq!(unsafe { libc::ftruncate(fd, data.len() as libc::off_t) }, 0);
        // SAFETY: the object was sized above and remains open through copy.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(mapped, libc::MAP_FAILED);
        // SAFETY: the mapping has exactly data.len() writable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast(), data.len());
            libc::munmap(mapped, data.len());
            libc::close(fd);
        }
        name
    }

    #[test]
    fn runtime_shared_memory_query_and_quiet_upload() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let mut replies = Vec::new();
        let probe = pane_test_shm("probe", &[1, 2, 3]);
        let query = format!(
            "\x1b_Ga=q,t=s,i=31,f=24,s=1,v=1,S=3;{}\x1b\\",
            STANDARD.encode(probe.as_bytes())
        );
        pane.process_output_for_runtime(
            query.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=31;OK\x1b\\");
        assert!(pane.image_store().get(31).is_none());

        replies.clear();
        let image = pane_test_shm("upload", &[4, 5, 6]);
        let upload = format!(
            "\x1b_Ga=t,t=s,i=32,f=24,s=1,v=1,S=3,q=2;{}\x1b\\",
            STANDARD.encode(image.as_bytes())
        );
        pane.process_output_for_runtime(
            upload.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert!(replies.is_empty());
        assert_eq!(pane.image_store().get(32).unwrap().data, [4, 5, 6]);

        let missing = format!(
            "\x1b_Ga=q,t=s,i=33,f=24,s=1,v=1,S=3;{}\x1b\\",
            STANDARD.encode(image.as_bytes())
        );
        pane.process_output_for_runtime(
            missing.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=33;EBADF:Failed to read image file\x1b\\");
    }

    #[test]
    fn runtime_compressed_shared_png_queries_and_failed_replacements_preserve_state() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[9, 10, 11, 255])
                .unwrap();
        }
        let compress = |data: &[u8]| {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        };
        let compressed = compress(&png);
        let cell = CellPixelSize::new(1, 1).unwrap();
        let send = |pane: &mut Pane, suffix: &str, controls: &str, bytes: &[u8]| {
            let mut data = b"xx".to_vec();
            data.extend_from_slice(bytes);
            data.extend_from_slice(b"unselected tail");
            let name = pane_test_shm(suffix, &data);
            let command = format!(
                "\x1b_G{controls},t=s,f=100,o=z,S={},O=2;{}\x1b\\\x1b[c",
                bytes.len(),
                STANDARD.encode(name.as_bytes())
            );
            let mut replies = Vec::new();
            pane.process_output_for_runtime(
                command.as_bytes(),
                &mut |reply| replies.extend_from_slice(reply),
                Some(cell),
                true,
            );
            // SAFETY: name is a valid NUL-terminated name, and a failed open
            // creates no descriptor. The completed transfer must have unlinked it.
            assert_eq!(
                unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) },
                -1
            );
            replies
        };
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        assert_eq!(
            send(&mut pane, "pngq", "a=q,i=36", &compressed),
            b"\x1b_Gi=36;OK\x1b\\\x1b[?1;0c"
        );
        assert!(pane.image_store().is_empty());
        assert_eq!(
            send(&mut pane, "pngT", "a=T,i=36,p=4,C=1", &compressed),
            b"\x1b_Gi=36,p=4;OK\x1b\\\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().get(36).unwrap().data, png);
        assert_eq!(
            pane.compose_image_snapshot(cell).unwrap().pixels[..4],
            [9, 10, 11, 255]
        );
        let revision = pane.image_store().revision();
        let cursor = pane.screen().cursor();
        let mut malformed = compressed.clone();
        malformed.push(0);
        let invalid_image = compress(b"invalid PNG");
        for (index, (controls, bytes, expected)) in [
            (
                "a=T,i=36,p=4",
                malformed.as_slice(),
                "EBADF:Failed to read image file",
            ),
            (
                "a=T,i=36,p=4",
                invalid_image.as_slice(),
                "EINVAL:invalid image",
            ),
            (
                "a=T,i=36,p=4,X=1",
                compressed.as_slice(),
                "EINVAL:invalid placement",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                send(&mut pane, &format!("pngerr{index}"), controls, bytes),
                format!("\x1b_Gi=36,p=4;{expected}\x1b\\\x1b[?1;0c").as_bytes()
            );
            assert_eq!(pane.image_store().get(36).unwrap().data, png);
            assert_eq!(pane.image_store().revision(), revision);
            assert_eq!(pane.screen().cursor(), cursor);
            assert_eq!(pane.image_store().placements().count(), 1);
        }
        assert_eq!(
            send(&mut pane, "pngquiet", "a=q,i=36,q=1", &compressed),
            b"\x1b[?1;0c"
        );
        assert_eq!(
            send(&mut pane, "pngfail", "a=T,i=36,p=4,q=2", &malformed),
            b"\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().revision(), revision);
    }

    #[test]
    fn runtime_tmux_wrapped_queries_uploads_and_deletes_use_pane_store() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let send = |pane: &mut Pane, command: &[u8]| {
            let mut wrapper = b"\x1bPtmux;".to_vec();
            for &byte in command {
                wrapper.push(byte);
                if byte == 0x1b {
                    wrapper.push(byte);
                }
            }
            wrapper.extend_from_slice(b"\x1b\\\x1b[c");
            let mut replies = Vec::new();
            for byte in wrapper {
                pane.process_output_for_runtime(
                    &[byte],
                    &mut |reply| replies.extend_from_slice(reply),
                    Some(cell),
                    true,
                );
            }
            replies
        };
        assert_eq!(
            send(&mut pane, b"\x1b_Ga=q,i=38,f=24,s=1,v=1;BAUG\x1b\\"),
            b"\x1b_Gi=38;OK\x1b\\\x1b[?1;0c"
        );
        assert!(pane.image_store().is_empty());
        assert_eq!(
            send(&mut pane, b"\x1b_Ga=T,i=38,p=4,f=24,s=1,v=1,C=1;BAUG\x1b\\"),
            b"\x1b_Gi=38,p=4;OK\x1b\\\x1b[?1;0c"
        );
        assert_eq!(
            pane.compose_image_snapshot(cell).unwrap().pixels[..4],
            [4, 5, 6, 255]
        );
        let revision = pane.image_store().revision();
        assert_eq!(
            send(&mut pane, b"\x1b_Ga=T,i=38,f=24,s=1,v=1,q=2;invalid\x1b\\"),
            b"\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.image_store().get(38).unwrap().data, [4, 5, 6]);
        assert_eq!(send(&mut pane, b"\x1b_Ga=d,d=I,i=38\x1b\\"), b"\x1b[?1;0c");
        assert!(pane.image_store().is_empty());
    }

    #[test]
    fn runtime_file_query_upload_and_failed_replacement_preserve_state() {
        use std::{io::Write, os::unix::ffi::OsStrExt};
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[4, 5, 6]).unwrap();
        let name = STANDARD.encode(file.path().as_os_str().as_bytes());
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        let mut send = |pane: &mut Pane, controls: &str| {
            let command = format!("\x1b_G{controls};{name}\x1b\\\x1b[c");
            pane.process_output_for_runtime(
                command.as_bytes(),
                &mut |reply| replies.extend_from_slice(reply),
                Some(cell),
                true,
            );
            std::mem::take(&mut replies)
        };
        assert_eq!(
            send(&mut pane, "a=q,t=f,i=31,f=24,s=1,v=1"),
            b"\x1b_Gi=31;OK\x1b\\\x1b[?1;0c"
        );
        assert!(pane.image_store().is_empty());
        assert_eq!(
            send(&mut pane, "a=T,t=f,i=31,p=9,f=24,s=1,v=1,C=1"),
            b"\x1b_Gi=31,p=9;OK\x1b\\\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().get(31).unwrap().data, [4, 5, 6]);
        assert_eq!(
            pane.compose_image_snapshot(cell).unwrap().pixels[..4],
            [4, 5, 6, 255]
        );
        let revision = pane.image_store().revision();
        let cursor = pane.screen().cursor();
        std::fs::write(file.path(), [7, 8, 9]).unwrap();
        assert_eq!(
            send(&mut pane, "a=q,t=f,i=31,f=24,s=1,v=1,q=1"),
            b"\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().get(31).unwrap().data, [4, 5, 6]);
        assert_eq!(pane.image_store().revision(), revision);
        // Read failure, invalid image and invalid placement all leave the
        // previous image, references and cursor intact.
        for (controls, expected) in [
            (
                "a=T,t=f,i=31,p=9,f=24,s=1,v=1,S=4",
                "EBADF:Failed to read image file",
            ),
            ("a=T,t=f,i=31,p=9,f=100", "EINVAL:invalid image"),
            (
                "a=T,t=f,i=31,p=9,f=24,s=1,v=1,X=1",
                "EINVAL:invalid placement",
            ),
        ] {
            assert_eq!(
                send(&mut pane, controls),
                format!("\x1b_Gi=31,p=9;{expected}\x1b\\\x1b[?1;0c").as_bytes()
            );
            assert_eq!(pane.image_store().get(31).unwrap().data, [4, 5, 6]);
            assert_eq!(pane.image_store().revision(), revision);
            assert_eq!(pane.screen().cursor(), cursor);
            assert_eq!(pane.image_store().placements().count(), 1);
        }
        assert_eq!(
            send(&mut pane, "a=T,t=f,i=31,p=9,f=24,s=1,v=1,S=4,q=2"),
            b"\x1b[?1;0c"
        );
        assert!(file.path().exists());
    }

    #[test]
    fn runtime_transmit_and_place_ack_follows_final_chunk_and_store_result() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=61,p=4,f=32,s=1,v=1,m=1,q=1,C=1;AQID\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(61).is_none());
        pane.process_output_for_runtime(
            b"X\x1b_Gm=0,q=0;BA==\x1b\\\x1b[c",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=61,p=4;OK\x1b\\\x1b[?1;0c");
        assert_eq!(pane.screen().cursor(), (0, 1));
        assert_eq!(pane.image_store().get(61).unwrap().data, [1, 2, 3, 4]);
        let placement = pane.image_store().placements().next().unwrap();
        assert_eq!(placement.placement_id, Some(4));
        assert_eq!(placement.geometry.unwrap().anchor.column, 1);
        let revision = pane.image_store().revision();

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=61,p=5,f=32,s=1,v=1,X=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=61,p=5;EINVAL:invalid placement\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.screen().cursor(), (0, 1));

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=61,p=6,f=100;YQ==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=61,p=6;EINVAL:invalid image\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=61,p=7,f=32,s=1,v=1,U=1,X=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=61,p=7;EINVAL:invalid placement\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=62,p=8,f=32,s=1,v=1,C=1,q=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(62).is_some());

        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=63,p=9,f=100,q=2;YQ==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(63).is_none());

        pane.process_output_for_runtime(
            b"\x1b_Ga=T,i=64,p=10,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(64).is_some());
    }

    #[test]
    fn runtime_rejects_unreadable_media_without_mutating() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,i=7,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\",
            &mut |_| {},
            Some(cell),
            true,
        );
        let revision = pane.image_store().revision();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,t=f,i=7,p=2,f=100;L3ByaXZhdGUvcGljLnBuZw==\x1b\\\x1b[c",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(
            replies,
            b"\x1b_Gi=7,p=2;EBADF:Failed to read image file\x1b\\\x1b[?1;0c"
        );
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.image_store().get(7).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(pane.image_store().placements().count(), 0);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,t=d,i=7,p=2,f=32,s=1,v=1,C=1;BAIDAg==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=2;OK\x1b\\");
        assert_eq!(pane.image_store().get(7).unwrap().data, [4, 2, 3, 2]);
        assert_eq!(pane.image_store().placements().count(), 1);
        let revision = pane.image_store().revision();

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=q,t=s,i=7,f=100;L25hbWU=\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert!(replies.is_empty());
        assert_eq!(pane.image_store().revision(), revision);
    }

    #[test]
    fn runtime_anonymous_uploads_display_without_child_replies() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,f=32,s=1,v=1,p=9,C=1;AQIDBA==\x1b\\\x1b_Ga=T,f=32,s=1,v=1,i=0,p=9,C=1;BAIDAg==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert_eq!(pane.image_store().len(), 2);
        assert_eq!(pane.image_store().placements().count(), 2);
        assert!(
            pane.image_store()
                .placements()
                .all(|placement| placement.placement_id.is_none())
        );
    }

    #[test]
    fn runtime_virtual_placements_ack_without_moving_cursor_or_drawing_pixels() {
        let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b[2;2H\x1b_Ga=T,f=32,s=1,v=1,i=7,p=2,U=1,c=1,r=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=2;OK\x1b\\");
        assert_eq!(pane.screen().cursor(), (1, 1));
        let placement = pane.image_store().placements().next().unwrap();
        assert!(placement.geometry.is_none());
        assert!(placement.virtual_layout.is_some());
        assert!(
            pane.compose_image_snapshot(cell)
                .unwrap()
                .pixels
                .iter()
                .all(|&byte| byte == 0)
        );

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,p=2,U=1,c=2,r=2\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=2;OK\x1b\\");
        assert_eq!(pane.screen().cursor(), (1, 1));
        assert_eq!(pane.image_store().placements().count(), 1);
        assert_eq!(
            pane.image_store()
                .placements()
                .next()
                .unwrap()
                .virtual_layout
                .unwrap()
                .columns,
            2
        );
    }

    #[test]
    fn runtime_virtual_place_infers_extent_from_previously_uploaded_png() {
        use base64::Engine;

        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255; 16])
                .unwrap();
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        let upload = format!("\x1b[2;2H\x1b_Ga=t,q=2,f=100,i=7;{encoded}\x1b\\");
        pane.process_output_for_runtime(
            upload.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,p=2,U=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=2;OK\x1b\\");
        assert_eq!(pane.screen().cursor(), (1, 1));
        let placement = pane.image_store().placements().next().unwrap();
        assert!(placement.geometry.is_none());
        let layout = placement.virtual_layout.unwrap();
        assert_eq!((layout.columns, layout.rows), (2, 2));

        pane.process_output_for_runtime(
            "\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let image = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&image.pixels[(3 + 1) * 4..(3 + 2) * 4], &[255; 4]);
    }

    #[test]
    fn small_virtual_png_samples_only_referenced_cells() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, 4, 4);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&source)
            .unwrap();
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let upload = format!("\x1b_Ga=t,f=100,i=7;{}\x1b\\", STANDARD.encode(png));
        pane.process_output_for_runtime(upload.as_bytes(), &mut |_| {}, Some(cell), true);
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,U=1,c=4,r=4\x1b\\",
            &mut |_| {},
            Some(cell),
            true,
        );
        pane.process_output_for_runtime(
            "\x1b[1;1H\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}\x1b[3;3H\u{10eeee}\u{030e}\u{030e}"
                .as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        let pixel = |row: usize, column: usize| {
            let start = (row * 4 + column) * 4;
            &snapshot.pixels[start..start + 4]
        };
        assert_eq!(pixel(0, 0), &[0, 0, 7, 255]);
        assert_eq!(pixel(2, 2), &[2, 2, 7, 255]);
        assert_eq!(pixel(1, 1), &[0, 0, 0, 0]);
    }

    #[test]
    fn small_interlaced_virtual_png_keeps_full_decode_fallback() {
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        info.interlaced = true;
        let mut png = Vec::new();
        png::Encoder::with_info(&mut png, info)
            .unwrap()
            .write_header()
            .unwrap()
            .write_image_data(&[11, 12, 13, 255])
            .unwrap();
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let upload = format!("\x1b_Ga=t,f=100,i=7;{}\x1b\\", STANDARD.encode(png));
        pane.process_output_for_runtime(upload.as_bytes(), &mut |_| {}, Some(cell), true);
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,U=1,c=1,r=1\x1b\\",
            &mut |_| {},
            Some(cell),
            true,
        );
        pane.process_output_for_runtime(
            "\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&snapshot.pixels[..4], &[11, 12, 13, 255]);
    }

    #[test]
    fn runtime_streams_large_png_into_cropped_regular_and_virtual_placements() {
        let width = 2900u32;
        let height = 2900u32;
        let mut source = vec![0; (width * height * 4) as usize];
        let split = (width * height / 2 * 4) as usize;
        for pixel in source[..split].as_chunks_mut::<4>().0 {
            *pixel = [255, 0, 0, 255];
        }
        for pixel in source[split..].as_chunks_mut::<4>().0 {
            *pixel = [0, 0, 255, 255];
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let encoded = STANDARD.encode(&png);
        assert!(encoded.len() > 100_000);
        let first = format!(
            "\x1b_Ga=t,f=100,i=7,q=2,s={width},v={height},m=1;{}\x1b\\",
            &encoded[..100_000]
        );
        let last = format!("\x1b_Gm=0;{}\x1b\\", &encoded[100_000..]);
        pane.process_output_for_runtime(first.as_bytes(), &mut |_| {}, Some(cell), true);
        pane.process_output_for_runtime(last.as_bytes(), &mut |_| {}, Some(cell), true);
        assert_eq!(
            pane.image_store().get(7).unwrap().decode_rgba(),
            Err(crate::graphics_decode::DecodeError::OutputLimit)
        );

        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b[3;3H\x1b_Ga=p,i=7,p=1,x=1449,y=1449,w=2,h=2,c=2,r=2,C=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=1;OK\x1b\\");
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        let at = |row: usize, column: usize| {
            &snapshot.pixels[(row * 4 + column) * 4..(row * 4 + column + 1) * 4]
        };
        assert_eq!(at(2, 2), &[255, 0, 0, 255]);
        assert_eq!(at(3, 2), &[0, 0, 255, 255]);
        assert_eq!(at(0, 0), &[0, 0, 0, 0]);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,p=2,U=1,x=1449,y=1449,w=2,h=2,c=2,r=2\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7,p=2;OK\x1b\\");
        pane.process_output_for_runtime(
            "\x1b[1;1H\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&snapshot.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &snapshot.pixels[(3 * 4 + 2) * 4..(3 * 4 + 3) * 4],
            &[0, 0, 255, 255]
        );

        let mut sparse_pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let large_cell = CellPixelSize::new(1024, 1024).unwrap();
        sparse_pane.process_output_for_runtime(
            first.as_bytes(),
            &mut |_| {},
            Some(large_cell),
            true,
        );
        sparse_pane.process_output_for_runtime(
            last.as_bytes(),
            &mut |_| {},
            Some(large_cell),
            true,
        );
        let mut sparse_replies = Vec::new();
        sparse_pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=7,p=3,U=1\x1b\\",
            &mut |reply| sparse_replies.extend_from_slice(reply),
            Some(large_cell),
            true,
        );
        assert_eq!(sparse_replies, b"\x1b_Gi=7,p=3;OK\x1b\\");
        sparse_pane.process_output_for_runtime(
            "\x1b[1;1H\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}\x1b[2;2H\u{10eeee}\u{030e}\u{030e}"
                .as_bytes(),
            &mut |_| {},
            Some(large_cell),
            true,
        );
        let sparse = sparse_pane.compose_image_snapshot(large_cell).unwrap();
        let stride = 2048usize * 4;
        assert_eq!(&sparse.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &sparse.pixels[1024 * stride + 1024 * 4..1024 * stride + 1024 * 4 + 4],
            &[0, 0, 255, 255]
        );
        assert_eq!(&sparse.pixels[1024 * 4..1024 * 4 + 4], &[0, 0, 0, 0]);

        let revision = pane.image_store().revision();
        *png.last_mut().unwrap() ^= 1;
        let encoded = STANDARD.encode(&png);
        let first = format!("\x1b_Ga=t,f=100,i=7,m=1;{}\x1b\\", &encoded[..100_000]);
        let last = format!("\x1b_Gm=0;{}\x1b\\", &encoded[100_000..]);
        replies.clear();
        pane.process_output_for_runtime(
            first.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        pane.process_output_for_runtime(
            last.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=7;EINVAL:invalid image\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(
            &pane.compose_image_snapshot(cell).unwrap().pixels[..4],
            &[255, 0, 0, 255]
        );
    }

    #[test]
    fn runtime_samples_only_visible_part_of_oversized_png_destination() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 4, 4);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        let command = format!(
            "\x1b_Ga=T,f=100,i=11,c=4096,r=4096,C=1;{}\x1b\\",
            STANDARD.encode(png)
        );
        pane.process_output_for_runtime(
            command.as_bytes(),
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=11;OK\x1b\\");
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(snapshot.pixels.len(), 4 * 4 * 4);
        assert_eq!(&snapshot.pixels[..4], &[0, 0, 7, 255]);
        assert_eq!(&snapshot.pixels[(4 * 4 - 1) * 4..], &[0, 0, 7, 255]);
    }

    #[test]
    fn runtime_samples_only_visible_part_of_oversized_raw_destination() {
        for (format, source, expected) in [
            (24, vec![3, 5, 7], [3, 5, 7, 255]),
            (32, vec![3, 5, 7, 41], [3, 5, 7, 41]),
        ] {
            let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
            let cell = CellPixelSize::new(1, 1).unwrap();
            let mut replies = Vec::new();
            let command = format!(
                "\x1b_Ga=T,f={format},i=11,s=1,v=1,c=4096,r=4096,C=1;{}\x1b\\",
                STANDARD.encode(source)
            );
            pane.process_output_for_runtime(
                command.as_bytes(),
                &mut |reply| replies.extend_from_slice(reply),
                Some(cell),
                true,
            );
            assert_eq!(replies, b"\x1b_Gi=11;OK\x1b\\");
            let snapshot = pane.compose_image_snapshot(cell).unwrap();
            assert_eq!(snapshot.pixels.len(), 4 * 4 * 4);
            assert!(
                snapshot
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| *pixel == expected)
            );
        }
    }

    #[test]
    fn runtime_samples_visible_placeholder_without_full_virtual_png_raster() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 4, 4);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[3, 5, 7, 255].repeat(16)).unwrap();
            writer.finish().unwrap();
        }
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let upload = format!("\x1b_Ga=t,f=100,i=12;{}\x1b\\", STANDARD.encode(png));
        pane.process_output_for_runtime(upload.as_bytes(), &mut |_| {}, Some(cell), true);
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=12,p=1,U=1,c=4096,r=4096\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=12,p=1;OK\x1b\\");
        pane.process_output_for_runtime(
            "\x1b[2;2H\x1b[38;5;12m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&snapshot.pixels[..4], &[0, 0, 0, 0]);
        assert_eq!(&snapshot.pixels[(4 + 1) * 4..(4 + 2) * 4], &[3, 5, 7, 255]);
    }

    #[test]
    fn runtime_streams_large_compressed_rgba_without_expanding_the_stored_image() {
        use flate2::{Compression, write::ZlibEncoder};

        fn send_compressed(
            pane: &mut Pane,
            controls: &str,
            compressed: &[u8],
            cell: CellPixelSize,
            replies: &mut Vec<u8>,
        ) {
            let encoded = STANDARD.encode(compressed);
            let chunks: Vec<_> = encoded.as_bytes().chunks(100_000).collect();
            for (index, chunk) in chunks.iter().enumerate() {
                let controls = if index == 0 { controls } else { "" };
                let more = u8::from(index + 1 != chunks.len());
                let command = format!(
                    "\x1b_G{controls}m={more};{}\x1b\\",
                    std::str::from_utf8(chunk).unwrap()
                );
                pane.process_output_for_runtime(
                    command.as_bytes(),
                    &mut |reply| replies.extend_from_slice(reply),
                    Some(cell),
                    true,
                );
            }
        }

        let width = 2900u32;
        let height = 2900u32;
        let red_row = [255, 0, 0, 255].repeat(width as usize);
        let blue_row = [0, 0, 255, 255].repeat(width as usize);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..height / 2 {
            encoder.write_all(&red_row).unwrap();
        }
        for _ in height / 2..height {
            encoder.write_all(&blue_row).unwrap();
        }
        let mut compressed = encoder.finish().unwrap();
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        send_compressed(
            &mut pane,
            &format!("a=T,o=z,s={width},v={height},i=9,p=1,x=1449,y=1449,w=2,h=2,c=2,r=2,C=1,"),
            &compressed,
            cell,
            &mut replies,
        );
        assert_eq!(replies, b"\x1b_Gi=9,p=1;OK\x1b\\");
        let image = pane.image_store().get(9).unwrap();
        assert_eq!(image.format, crate::graphics_store::ImageFormat::RgbaZlib);
        assert_eq!(image.data, compressed);
        assert_eq!(
            image.decode_rgba(),
            Err(crate::graphics_decode::DecodeError::OutputLimit)
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&snapshot.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(&snapshot.pixels[4 * 4..4 * 4 + 4], &[0, 0, 255, 255]);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=9,p=2,U=1,c=2900,r=2900\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=9,p=2;OK\x1b\\");
        pane.process_output_for_runtime(
            "\x1b[4;4H\x1b[38;5;9m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            Some(cell),
            true,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&snapshot.pixels[(4 * 4 - 1) * 4..], &[255, 0, 0, 255]);

        let mut sparse_pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let large_cell = CellPixelSize::new(1024, 1024).unwrap();
        let mut sparse_replies = Vec::new();
        send_compressed(
            &mut sparse_pane,
            &format!("a=t,o=z,s={width},v={height},i=9,"),
            &compressed,
            large_cell,
            &mut sparse_replies,
        );
        sparse_pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=9,p=2,U=1\x1b\\",
            &mut |reply| sparse_replies.extend_from_slice(reply),
            Some(large_cell),
            true,
        );
        assert_eq!(sparse_replies, b"\x1b_Gi=9;OK\x1b\\\x1b_Gi=9,p=2;OK\x1b\\");
        sparse_pane.process_output_for_runtime(
            "\x1b[1;1H\x1b[38;5;9m\u{10eeee}\u{0305}\u{0305}\x1b[2;2H\u{10eeee}\u{030e}\u{030e}"
                .as_bytes(),
            &mut |_| {},
            Some(large_cell),
            true,
        );
        let sparse = sparse_pane.compose_image_snapshot(large_cell).unwrap();
        let stride = 2048usize * 4;
        assert_eq!(&sparse.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &sparse.pixels[1024 * stride + 1024 * 4..1024 * stride + 1024 * 4 + 4],
            &[0, 0, 255, 255]
        );
        assert_eq!(&sparse.pixels[1024 * 4..1024 * 4 + 4], &[0, 0, 0, 0]);

        let revision = pane.image_store().revision();
        *compressed.last_mut().unwrap() ^= 1;
        replies.clear();
        send_compressed(
            &mut pane,
            &format!("a=t,o=z,s={width},v={height},i=9,"),
            &compressed,
            cell,
            &mut replies,
        );
        assert_eq!(replies, b"\x1b_Gi=9;EINVAL:invalid image\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(
            &pane.compose_image_snapshot(cell).unwrap().pixels[..4],
            &[255, 0, 0, 255]
        );
    }

    #[test]
    fn virtual_place_placeholder_extent_follows_current_cell_pixels() {
        use base64::Engine;

        let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
        let original_cell = CellPixelSize::new(1, 1).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode([255; 16]);
        let upload = format!("\x1b_Ga=T,f=32,s=2,v=2,i=7,p=1,U=1;{encoded}\x1b\\");
        pane.process_output_with_image_store_sized(upload.as_bytes(), &mut |_| {}, original_cell);
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;1m\x1b[1;1H\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}"
                .as_bytes(),
            &mut |_| {},
            original_cell,
        );

        let original = pane.compose_image_snapshot(original_cell).unwrap();
        assert_eq!(&original.pixels[4..8], &[255; 4]);

        let wider_cell = CellPixelSize::new(2, 1).unwrap();
        let resized = pane.compose_image_snapshot(wider_cell).unwrap();
        assert_eq!(&resized.pixels[0..4], &[255; 4]);
        assert_eq!(&resized.pixels[8..12], &[0; 4]);
    }

    #[test]
    fn out_of_bounds_virtual_placeholder_does_not_decode_image() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=100,i=7,p=1,U=1,c=1,r=1;AQ==\x1b\\",
            &mut |_| {},
            cell,
        );
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;1m\u{10eeee}\u{0305}\u{030d}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(
            pane.compose_image_snapshot(cell)
                .unwrap()
                .pixels
                .iter()
                .all(|&pixel| pixel == 0)
        );

        pane.process_output_with_image_store_sized(
            "\x1b[1;1H\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(pane.compose_image_snapshot(cell).is_err());
    }

    #[test]
    fn inferred_bounds_do_not_trust_unvalidated_png_dimensions() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=100,s=1,v=1,i=7,p=1,U=1;AQ==\x1b\\",
            &mut |_| {},
            cell,
        );
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;1m\u{10eeee}\u{0305}\u{030d}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert_eq!(pane.image_store().placements().count(), 1);
        assert!(pane.compose_image_snapshot(cell).is_err());
    }

    #[test]
    fn out_of_bounds_virtual_placeholder_avoids_over_budget_resample() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(3000, 1).unwrap();
        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,U=1,c=1;/////w==\x1b\\",
            &mut |_| {},
            cell,
        );
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;1m\u{10eeee}\u{0305}\u{030d}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(
            pane.compose_image_snapshot(cell)
                .unwrap()
                .pixels
                .iter()
                .all(|&pixel| pixel == 0)
        );
    }

    #[test]
    fn inferred_virtual_extent_skips_out_of_bounds_cell_before_resampling() {
        use base64::Engine;

        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let initial_cell = CellPixelSize::new(1, 1).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode([255; 16]);
        let upload = format!("\x1b_Ga=T,f=32,s=2,v=2,i=7,p=1,U=1;{encoded}\x1b\\");
        pane.process_output_with_image_store_sized(upload.as_bytes(), &mut |_| {}, initial_cell);
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;1m\u{10eeee}\u{0305}\u{030d}\u{10eeee}\u{0305}\u{0305}"
                .as_bytes(),
            &mut |_| {},
            initial_cell,
        );

        let resized = pane
            .compose_image_snapshot(CellPixelSize::new(2, 1).unwrap())
            .unwrap();
        assert_eq!(&resized.pixels[0..4], &[0; 4]);
        assert_eq!(&resized.pixels[8..12], &[255; 4]);
    }

    #[test]
    fn unicode_placeholders_draw_only_their_own_image_cells_and_follow_text_edits() {
        use base64::Engine;

        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let source = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let encoded = base64::engine::general_purpose::STANDARD.encode(source);
        let upload = format!("\x1b_Ga=T,f=32,s=2,v=2,i=42,p=1,U=1,c=2,r=2,z=0;{encoded}\x1b\\");
        pane.process_output_with_image_store_sized(upload.as_bytes(), &mut |_| {}, cell);
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;42m\x1b[58;5;1m\x1b[1;1H\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\x1b[2;1H\u{10eeee}\u{030d}\u{0305}\u{10eeee}\u{030d}\u{030d}"
                .as_bytes(),
            &mut |_| {},
            cell,
        );
        let snapshot = pane.compose_image_snapshot(cell).unwrap();
        let at = |row: usize, column: usize| {
            &snapshot.pixels[(row * 4 + column) * 4..(row * 4 + column + 1) * 4]
        };
        assert_eq!(at(0, 0), [255, 0, 0, 255]);
        assert_eq!(at(0, 1), [0, 255, 0, 255]);
        assert_eq!(at(1, 0), [0, 0, 255, 255]);
        assert_eq!(at(1, 1), [255, 255, 255, 255]);
        assert_eq!(at(2, 2), [0, 0, 0, 0]);

        let revision = pane.virtual_placeholder_revision();
        pane.process_output_with_image_store_sized(
            "\x1b[1;2H \x1b[3;3H\u{10eeee}\u{0305}\u{030d}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert_ne!(pane.virtual_placeholder_revision(), revision);
        let moved = pane.compose_image_snapshot(cell).unwrap();
        assert_eq!(&moved.pixels[4..8], [0, 0, 0, 0]);
        assert_eq!(
            &moved.pixels[(2 * 4 + 2) * 4..(2 * 4 + 3) * 4],
            [0, 255, 0, 255]
        );

        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=d,d=i,i=42,p=1\x1b\\",
            &mut |_| {},
            cell,
        );
        assert!(
            pane.compose_image_snapshot(cell)
                .unwrap()
                .pixels
                .iter()
                .all(|&p| p == 0)
        );
    }

    #[test]
    fn placeholder_underline_color_selects_a_named_virtual_placement() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=t,f=32,s=1,v=1,i=7;AQIDBA==\x1b\\\x1b_Ga=p,i=7,p=1,U=1,c=1,r=1,z=-1\x1b\\\x1b_Ga=p,i=7,p=2,U=1,c=1,r=1,z=0\x1b\\",
            &mut |_| {},
            cell,
        );
        pane.process_output_with_image_store_sized(
            "\x1b[38;5;7m\x1b[58;5;2m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(
            pane.compose_image_band(cell, ImageBand::AboveText)
                .unwrap()
                .is_some()
        );
        assert!(
            pane.compose_image_band(cell, ImageBand::BehindText)
                .unwrap()
                .is_none()
        );

        pane.process_output_with_image_store_sized(
            "\x1b[1;1H\x1b[58;5;1m\u{10eeee}\u{0305}\u{0305}".as_bytes(),
            &mut |_| {},
            cell,
        );
        assert!(
            pane.compose_image_band(cell, ImageBand::AboveText)
                .unwrap()
                .is_none()
        );
        assert!(
            pane.compose_image_band(cell, ImageBand::BehindText)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn runtime_numbered_upload_allocates_id_and_replies_after_final_chunk() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,i=1,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,I=13,p=9,f=32,s=1,v=1,C=1,m=1;AQID\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert!(replies.is_empty());
        pane.process_output_for_runtime(
            b"\x1b_Gm=0;BA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=2,I=13,p=9;OK\x1b\\");
        assert_eq!(pane.image_store().get(2).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(pane.image_store().placements().next().unwrap().image_id, 2);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,I=13,f=32,s=1,v=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=3,I=13;OK\x1b\\");
        assert!(pane.image_store().get(2).is_some());

        replies.clear();
        let revision = pane.image_store().revision();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,I=14,f=100;YQ==\x1b\\\x1b_Ga=t,i=4,I=14,f=32,s=1,v=1;AQIDBA==\x1b\\\x1b_Ga=t,I=0,f=32,s=1,v=1;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            true,
        );
        assert_eq!(
            replies,
            b"\x1b_GI=14;EINVAL:invalid image\x1b\\\x1b_Gi=4,I=14;EINVAL:invalid image\x1b\\\x1b_GI=0;EINVAL:invalid image\x1b\\"
        );
        assert_eq!(pane.image_store().revision(), revision);
        assert!(pane.image_store().get(4).is_none());
    }

    #[test]
    fn runtime_numbered_placement_uses_newest_live_image_and_replies_with_id() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,I=13,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\\x1b_Ga=t,I=13,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,I=13,p=1,C=1\x1b\\\x1b_Ga=p,i=1,p=2,C=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(
            replies,
            b"\x1b_Gi=2,I=13,p=1;OK\x1b\\\x1b_Gi=1,p=2;OK\x1b\\"
        );
        assert_eq!(
            pane.image_store()
                .placements()
                .map(|p| p.image_id)
                .collect::<Vec<_>>(),
            [2, 1]
        );

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=d,d=I,i=2\x1b\\\x1b_Ga=p,I=13,p=3,C=1\x1b\\\x1b_Ga=d,d=I,i=1\x1b\\\x1b_Ga=p,I=13,p=4,C=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(
            replies,
            b"\x1b_Gi=1,I=13,p=3;OK\x1b\\\x1b_GI=13,p=4;ENOENT:image not found\x1b\\"
        );
        assert!(pane.image_store().is_empty());
    }

    #[test]
    fn runtime_placement_ack_follows_store_result_and_precedes_da() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,i=51,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=51,p=3,c=1,r=1\x1b\\\x1b[c",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=51,p=3;OK\x1b\\\x1b[?1;0c");
        assert_eq!(pane.screen().cursor(), (1, 1));
        let revision = pane.image_store().revision();

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=999,p=8,c=1,r=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=999,p=8;ENOENT:image not found\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.screen().cursor(), (1, 1));

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=51,p=4,X=1,c=1,r=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=51,p=4;EINVAL:invalid placement\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=51,p=4,U=1,x=99\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=51,p=4;EINVAL:invalid placement\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=51,p=5,C=1,q=1\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert_eq!(pane.image_store().placements().count(), 2);

        pane.process_output_for_runtime(
            b"\x1b_Ga=p,i=999,p=9,q=2\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
    }

    #[test]
    fn runtime_upload_ack_follows_final_chunk_and_does_not_claim_failed_storage() {
        let mut pane = Pane::spawn("/bin/sh", 3, 3).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,f=32,s=1,v=1,i=44,m=1,q=1;AQID\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(44).is_none());
        pane.process_output_for_runtime(
            b"\x1b_Gm=0,q=0;BA==\x1b\\\x1b[c",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=44;OK\x1b\\\x1b[?1;0c");
        assert_eq!(pane.image_store().get(44).unwrap().data, [1, 2, 3, 4]);
        let revision = pane.image_store().revision();

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,f=100,i=44,q=1;YQ==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=44;EINVAL:invalid image\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.image_store().get(44).unwrap().data, [1, 2, 3, 4]);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,f=32,s=1,v=1,i=44,p=1;AAAAAA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert_eq!(replies, b"\x1b_Gi=44;EINVAL:invalid image\x1b\\");
        assert_eq!(pane.image_store().revision(), revision);

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b_Ga=t,f=100,i=44,q=2;YQ==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            true,
        );
        assert!(replies.is_empty());
        assert_eq!(pane.image_store().revision(), revision);

        pane.process_output_for_runtime(
            b"\x1b_Ga=t,f=32,s=1,v=1,i=45;AAAAAA==\x1b\\",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert!(replies.is_empty());
        assert!(pane.image_store().get(45).is_some());
    }

    #[test]
    fn runtime_rejects_invalid_png_without_replacing_image_or_moving_cursor() {
        let mut pane = Pane::spawn("/bin/sh", 4, 4).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=1,c=1,r=1;AQIDBA==\x1b\\",
            &mut |_| {},
            Some(cell),
            false,
        );
        let revision = pane.image_store().revision();
        let cursor = pane.screen().cursor();
        pane.process_output_for_runtime(
            b"\x1b_Ga=T,f=100,i=7,p=2,c=1,r=1;QQ==\x1b\\",
            &mut |_| {},
            Some(cell),
            false,
        );
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.image_store().get(7).unwrap().data, [1, 2, 3, 4]);
        assert_eq!(pane.image_store().placements().count(), 1);
        assert_eq!(pane.screen().cursor(), cursor);

        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0; 6])
                .unwrap();
        }
        let valid = format!("\x1b_Ga=T,f=100,i=7,p=3;{}\x1b\\", STANDARD.encode(png));
        pane.process_output_for_runtime(valid.as_bytes(), &mut |_| {}, Some(cell), false);
        assert_ne!(pane.image_store().revision(), revision);
        let placements: Vec<_> = pane.image_store().placements().collect();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placement_id, Some(3));
        assert_eq!(placements[0].geometry.unwrap().columns, Some(2));
        assert_eq!(placements[0].geometry.unwrap().rows, Some(1));
    }

    #[test]
    fn kitty_child_query_replies_before_da_without_mutating_stored_images() {
        let mut pane = Pane::spawn("/bin/sh", 2, 2).unwrap();
        let cell = CellPixelSize::new(1, 1).unwrap();
        pane.process_output_with_image_store_sized(
            b"\x1b_Ga=t,f=32,s=1,v=1,i=31;AQIDBA==\x1b\\",
            &mut |_| {},
            cell,
        );
        let revision = pane.image_store().revision();
        let mut replies = Vec::new();
        let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
        for part in query.chunks(3) {
            pane.process_output_for_runtime(
                part,
                &mut |reply| replies.extend_from_slice(reply),
                Some(cell),
                true,
            );
        }
        assert_eq!(replies, b"\x1b_Gi=31;OK\x1b\\\x1b[?1;0c");
        assert_eq!(pane.image_store().revision(), revision);
        assert_eq!(pane.image_store().get(31).unwrap().data, [1, 2, 3, 4]);

        replies.clear();
        pane.process_output_for_runtime(
            query,
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert_eq!(replies, b"\x1b[?1;0c");
        assert_eq!(pane.image_store().revision(), revision);
    }

    #[test]
    fn pane_pixel_size_replies_follow_runtime_resize_and_exact_cell_size() {
        let mut pane = Pane::spawn("/bin/sh", 2, 8).unwrap();
        let cell = CellPixelSize::new(12, 20).unwrap();
        let mut replies = Vec::new();
        pane.process_output_for_runtime(
            b"\x1b[14t\x1b[15t\x1b[16t",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert_eq!(replies, b"\x1b[4;40;96t\x1b[5;40;96t\x1b[6;20;12t");

        pane.prepare_resize(3, 5).unwrap().commit().unwrap();
        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b[15t\x1b[19t",
            &mut |reply| replies.extend_from_slice(reply),
            Some(cell),
            false,
        );
        assert_eq!(replies, b"\x1b[5;60;60t\x1b[9;3;5t");

        replies.clear();
        pane.process_output_for_runtime(
            b"\x1b[14t\x1b[15t\x1b[16t\x1b[18t",
            &mut |reply| replies.extend_from_slice(reply),
            None,
            false,
        );
        assert_eq!(replies, b"\x1b[8;3;5t");
    }

    #[test]
    fn graphics_payload_does_not_enter_semantic_command_output() {
        let mut pane = Pane::spawn("/bin/sh", 4, 40).unwrap();
        for part in [
            b"\x1b]133;C\x07before\x9fGf=100;U0VD".as_slice(),
            b"UkVU\x9cafter\x1b]133;D\x07",
        ] {
            pane.process_output(part, &mut |_| {});
        }
        assert_eq!(pane.last_command_output().as_deref(), Some("beforeafter"));
        assert_eq!(pane.screen().row(0).unwrap()[0].character, 'b');
    }

    #[test]
    fn input_and_reply_capacity_stop_at_lifecycle_boundaries() {
        let mut state = PaneIo::default();
        assert!(state.accepts_input());
        assert_eq!(state.reply_read_limit(), INPUT_LIMIT / MAX_REPLY_BYTES);
        state.to_shell.resize(INPUT_LIMIT - MAX_REPLY_BYTES, b'x');
        assert_eq!(state.reply_read_limit(), 1);
        state.to_shell.push_back(b'y');
        assert_eq!(state.reply_read_limit(), 0);
        assert!(state.accepts_input());
        state.to_shell.resize(INPUT_LIMIT, b'z');
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), 0);
        // Reaping a child must allow draining final output even with a full queue.
        state.status = Some(ExitStatus::from_raw(0));
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), MAX_REPLY_DRAIN_BYTES);
        state.eof = true;
        assert_eq!(state.reply_read_limit(), 0);
        state.status = None;
        state.to_shell.clear();
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), 0);
    }

    #[test]
    fn focus_and_removal_do_not_mix_queues_deadlines_or_exit_status() {
        let mut windows = Windows::default();
        let a = windows.create("a".into(), PaneIo::default()).unwrap();
        let b = windows.create("b".into(), PaneIo::default()).unwrap();
        let started = Instant::now() - Duration::from_millis(100);
        let first = windows.get_mut(a).unwrap().content_mut();
        first.to_shell.extend(b"keyboard\x1b[3;4R");
        first.synchronized_since = Some(started);
        first.eof_at = Some(started);
        first.eof = true;
        first.status = Some(ExitStatus::from_raw(7 << 8));
        first.dirty = false;
        windows.select(a).unwrap();
        windows.select_next();
        let second = windows.active_mut().unwrap().content_mut();
        assert!(second.to_shell.is_empty());
        assert!(second.accepts_input());
        assert!(second.dirty);
        assert!(second.synchronized_since.is_none());
        assert!(second.eof_at.is_none());
        second.to_shell.extend(b"other");
        let removed = windows.close(a).unwrap().into_content();
        assert_eq!(
            removed.to_shell.into_iter().collect::<Vec<_>>(),
            b"keyboard\x1b[3;4R"
        );
        assert_eq!(removed.synchronized_since, Some(started));
        assert_eq!(removed.eof_at, Some(started));
        assert_eq!(removed.status.unwrap().code(), Some(7));
        assert_eq!(windows.active().unwrap().id(), b);
        assert_eq!(
            windows
                .active()
                .unwrap()
                .content()
                .to_shell
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            b"other"
        );
    }
}
