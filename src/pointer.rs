//! Bounded OSC 22 pointer stacks. Names are canonical, immutable protocol values.

const DEPTH: usize = 16;
const SHAPES: &[&str] = &[
    "alias",
    "cell",
    "copy",
    "crosshair",
    "default",
    "e-resize",
    "ew-resize",
    "grab",
    "grabbing",
    "help",
    "move",
    "n-resize",
    "ne-resize",
    "nesw-resize",
    "no-drop",
    "not-allowed",
    "ns-resize",
    "nw-resize",
    "nwse-resize",
    "pointer",
    "progress",
    "s-resize",
    "se-resize",
    "sw-resize",
    "text",
    "vertical-text",
    "w-resize",
    "wait",
    "zoom-in",
    "zoom-out",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Stack {
    // None is an explicit reset at the top of an otherwise nonempty stack.
    shapes: Vec<Option<&'static str>>,
}

impl Stack {
    pub(crate) fn current(&self) -> Option<&'static str> {
        self.shapes.last().copied().flatten()
    }

    pub(crate) fn clear(&mut self) {
        self.shapes.clear();
    }

    pub(crate) fn apply(&mut self, value: &str, reply: &mut impl FnMut(&[u8])) {
        if let Some(query) = value.strip_prefix('?') {
            let answers: Vec<_> = query
                .split(',')
                .map(|name| match name {
                    "__current__" => self.current().unwrap_or("0"),
                    // These are Rustmux's virtual terminal defaults, not an outer
                    // terminal capability probe or its configured pointer options.
                    "__default__" | "__grabbed__" => "default",
                    _ if shape(name).is_some() => "1",
                    _ => "0",
                })
                .collect();
            let response = format!("\x1b]22;{}\x1b\\", answers.join(","));
            debug_assert!(response.len() <= crate::parser::MAX_REPLY_BYTES);
            reply(response.as_bytes());
        } else if let Some(names) = value.strip_prefix('>') {
            for name in names.split(',') {
                let selected = if name.is_empty() {
                    None
                } else if let Some(shape) = shape(name) {
                    Some(shape)
                } else {
                    continue;
                };
                if self.shapes.len() == DEPTH {
                    self.shapes.remove(0);
                }
                self.shapes.push(selected);
            }
        } else if value.starts_with('<') {
            self.shapes.pop(); // Names after '<' are ignored by the protocol.
        } else {
            let name = value.strip_prefix('=').unwrap_or(value);
            let selected = if name.is_empty() {
                None
            } else if let Some(shape) = shape(name) {
                Some(shape)
            } else {
                return;
            };
            // Setting replaces the current entry, preserving older pushes.
            if let Some(top) = self.shapes.last_mut() {
                *top = selected;
            } else {
                self.shapes.push(selected);
            }
        }
    }
}

fn shape(name: &str) -> Option<&'static str> {
    SHAPES.iter().copied().find(|known| *known == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_resets_and_pops_preserve_older_entries_and_unknown_names() {
        let mut stack = Stack::default();
        let mut replies = Vec::new();
        let mut query = |reply: &[u8]| replies.extend_from_slice(reply);
        for value in [
            "pointer",
            ">wait,crosshair",
            "=text",
            "=",
            "?__current__",
            "<ignored",
            "?__current__",
            "<",
            "?__current__",
            "unknown",
            "?__current__",
            "<",
            "<",
            "?__current__",
        ] {
            stack.apply(value, &mut query);
        }
        assert_eq!(replies, b"\x1b]22;0\x1b\\\x1b]22;wait\x1b\\\x1b]22;pointer\x1b\\\x1b]22;pointer\x1b\\\x1b]22;0\x1b\\");
        assert!(stack.shapes.is_empty());
    }

    #[test]
    fn local_overlays_clear_only_their_cloned_pointer_state() {
        use crate::{parser::Parser, screen::Screen};
        let mut child = Screen::new(24, 80).unwrap();
        Parser::new().advance(&mut child, b"\x1b]22;pointer\x1b\\");
        let before = child.clone();
        let prompt = crate::prompt::WindowPrompt::close().overlay(&child);
        let help = crate::shortcut_help::ShortcutHelp::new(false).overlay(&child);
        let history = crate::history_view::HistoryView::new(&child)
            .unwrap()
            .render()
            .unwrap();
        for overlay in [prompt, help, history] {
            assert_eq!(overlay.pointer_shape(), None);
        }
        assert_eq!(child, before);
    }

    #[test]
    fn bounded_stack_evicts_oldest_and_mixed_queries_are_local() {
        let mut stack = Stack::default();
        for name in SHAPES {
            stack.apply(&format!(">{name}"), &mut |_| panic!("set replied"));
        }
        assert_eq!(stack.shapes.len(), DEPTH);
        assert_eq!(stack.shapes[0], Some("no-drop"));
        let before = stack.clone();
        let mut replies = Vec::new();
        stack.apply(
            "?__current__,__default__,__grabbed__,pointer,invalid",
            &mut |reply| replies.extend_from_slice(reply),
        );
        assert_eq!(replies, b"\x1b]22;zoom-out,default,default,1,0\x1b\\");
        assert_eq!(stack, before);
        for _ in 0..DEPTH {
            stack.apply("<", &mut |_| {});
        }
        assert_eq!(stack.current(), None);
        stack.apply(">wait,unknown,pointer", &mut |_| {});
        assert_eq!(stack.shapes, [Some("wait"), Some("pointer")]);
        stack.apply(">", &mut |_| {});
        assert_eq!(stack.current(), None);
        stack.apply("<", &mut |_| {});
        assert_eq!(stack.current(), Some("pointer"));
    }
}
