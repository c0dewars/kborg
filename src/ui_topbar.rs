//! The workspace header exposes only the keyboard-help affordance.
pub fn help_at(width: u16, x: u16) -> bool {
    width >= 64 && x >= width - 4 && x < width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_visible_help_affordance_is_clickable() {
        for width in 1..180 {
            for x in 0..=width {
                assert_eq!(
                    help_at(width, x),
                    width >= 64 && x < width && x >= width - 4
                );
            }
        }
    }
}
