//! Battery state from /sys/class/power_supply. STUB: to be implemented.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// The first `type=Battery` supply, or None on a machine without one.
pub fn read() -> Option<Battery> {
    None
}
