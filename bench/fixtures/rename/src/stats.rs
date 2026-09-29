use crate::config::old_name;

// old_name_total is a different identifier.
pub fn old_name_total() -> u32 {
    old_name() + 1
}
