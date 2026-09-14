// ABOUTME: Commands a host dispatches to report something it did or measured
// (its screen width at startup, how a terminal it ran ended), so the reducer,
// not the host, decides what state changes. Unbound keymap rows, like the
// pointer commands.

use serde::Deserialize;
use serde_json::json;

use crate::app::events::AppEvent;
use crate::app::intent::{Args, Intent};
use crate::app::keymap::CommandId;

/// Command ids of the report rows.
pub mod ids {
    /// `{"columns": u16}`
    pub const MIGRATE_LAYOUT_WIDTHS: &str = "global.migrate_layout_widths";

    /// Every report command id.
    pub const ALL: &[&str] = &[MIGRATE_LAYOUT_WIDTHS];
}

/// Report the host's screen width so layout widths saved as column counts
/// become fractions of it.
#[must_use]
pub fn migrate_layout_widths(columns: u16) -> Intent {
    Intent::Command(
        CommandId::new(ids::MIGRATE_LAYOUT_WIDTHS),
        json!({ "columns": columns }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnsArgs {
    columns: u16,
}

/// The event a report row runs with `args` as its payload, with the same
/// contract as [`crate::app::pointer::with_args`].
pub(crate) fn with_args(event: &AppEvent, args: &Args) -> Option<Option<AppEvent>> {
    let parse = |args: &Args| serde_json::from_value::<ColumnsArgs>(args.clone()).ok();
    Some(match event {
        AppEvent::MigrateLayoutWidths { .. } => {
            parse(args).map(|args| AppEvent::MigrateLayoutWidths {
                columns: args.columns,
            })
        }
        _ => return None,
    })
}
