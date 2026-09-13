// ABOUTME: Cell geometry shared by renderers and state. State that remembers
// where the renderer last drew something (for hit-testing a click or a drag)
// stores an `Area`, never a renderer's own rectangle type.

/// A rectangle on the host's render surface, in cells from the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Area {
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the cell at (`x`, `y`) falls inside this area. The right and
    /// bottom edges are exclusive.
    #[must_use]
    pub const fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && x < self.x.saturating_add(self.width)
            && y >= self.y
            && y < self.y.saturating_add(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_excludes_the_right_and_bottom_edges_and_saturates() {
        let area = Area::new(2, 3, 4, 5);
        assert!(area.contains(2, 3));
        assert!(area.contains(5, 7));
        assert!(!area.contains(6, 3));
        assert!(!area.contains(2, 8));
        assert!(!area.contains(1, 3));
        assert!(Area::new(u16::MAX - 1, 0, 10, 1).contains(u16::MAX - 1, 0));
    }
}
