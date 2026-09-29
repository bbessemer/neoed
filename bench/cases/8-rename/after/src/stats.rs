use crate::config::new_name;

// old_name_total is a different identifier.
pub fn old_name_total() -> u32 {
    new_name() + 1
}
