// ABOUTME: The size of the surface the host renders into, published by the host.
// Renderer-agnostic code that lays text out to a width reads it here instead of
// asking a terminal library, so it runs the same under any renderer.

use std::sync::atomic::{AtomicU16, Ordering};

/// Columns the host last published; 0 until it publishes.
static COLUMNS: AtomicU16 = AtomicU16::new(0);

/// Record the width, in columns, of the surface the host renders into.
///
/// The terminal host calls this at startup and on every resize.
pub fn set_columns(columns: u16) {
    COLUMNS.store(columns, Ordering::Relaxed);
}

/// The width the host last published, or `None` before it has published one.
#[must_use]
pub fn columns() -> Option<u16> {
    match COLUMNS.load(Ordering::Relaxed) {
        0 => None,
        columns => Some(columns),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_width_is_read_back_and_zero_reads_as_unpublished() {
        set_columns(0);
        assert_eq!(columns(), None);
        set_columns(132);
        assert_eq!(columns(), Some(132));
        set_columns(0);
    }
}
