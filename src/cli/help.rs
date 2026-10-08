//! Help presentation; command names, descriptions and aliases come from clap.

use std::fmt::Write;

use clap::Subcommand;
use clap::builder::{StyledStr, Styles, styling::AnsiColor};

pub(super) fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Green.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Cyan.on_default().bold())
        .placeholder(AnsiColor::Yellow.on_default())
        .context(AnsiColor::White.on_default().dimmed())
}

pub(super) fn root_template() -> StyledStr {
    // Augment the command enum rather than Cli, which installs this template.
    let mut command = super::Command::augment_subcommands(clap::Command::new("rustmux"));
    command.build();
    let mut template = StyledStr::from("{about-with-newline}\n{usage-heading} {usage}\n\n");
    write_commands(&mut template, &command, true);
    write_commands(&mut template, &command, false);
    let header = *styles().get_header();
    let _ = write!(template, "{header}Options:{header:#}\n{{options}}");
    template
}

fn write_commands(template: &mut StyledStr, command: &clap::Command, groups: bool) {
    let commands: Vec<_> = command
        .get_subcommands()
        .filter(|child| {
            !child.is_hide_set()
                && (child.has_subcommands() && child.get_name() != "help") == groups
        })
        .collect();
    let width = commands
        .iter()
        .map(|child| child.get_name().len() + if groups { " <COMMAND>".len() } else { 0 })
        .max()
        .unwrap_or(0);
    let palette = styles();
    let header = palette.get_header();
    let literal = if groups {
        AnsiColor::Magenta.on_default().bold()
    } else {
        *palette.get_literal()
    };
    let placeholder = palette.get_placeholder();
    let heading = if groups { "Command groups" } else { "Commands" };
    let _ = writeln!(template, "{header}{heading}:{header:#}");
    for child in commands {
        let name = child.get_name();
        let _ = write!(template, "  {literal}{name}{literal:#}");
        let mut length = name.len();
        if groups {
            let _ = write!(template, " {placeholder}<COMMAND>{placeholder:#}");
            length += " <COMMAND>".len();
        }
        let about = child.get_about().cloned().unwrap_or_default();
        let _ = write!(
            template,
            "{:padding$}{about}",
            "",
            padding = width - length + 2
        );
        let aliases: Vec<_> = child.get_visible_aliases().collect();
        if !aliases.is_empty() {
            let context = palette.get_context();
            let plural = if aliases.len() == 1 { "" } else { "es" };
            let aliases = aliases.join(", ");
            let _ = write!(
                template,
                " {context}[alias{plural}: {context:#}{literal}{aliases}{literal:#}{context}]{context:#}"
            );
        }
        template.push_str("\n");
    }
    template.push_str("\n");
}
