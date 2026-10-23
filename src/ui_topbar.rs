//! Compact command navigation inspired by k10s; command names retain k9s grammar.
pub struct Item {
    pub x: u16,
    pub width: u16,
    pub command: &'static str,
    pub label: &'static str,
}
pub fn items(width: u16) -> Vec<Item> {
    let mut x = 9;
    let mut items = Vec::new();
    for (command, label) in [
        ("pods", ":pods"),
        ("deploy", ":deploy"),
        ("svc", ":svc"),
        ("nodes", ":nodes"),
        ("helm", ":helm"),
        ("pf", ":pf"),
        ("pulses", ":pulses"),
        ("ctx", ":ctx"),
        ("themes", ":themes"),
        ("help", "?"),
    ] {
        let item_width = label.len() as u16 + 2;
        if x + item_width > width {
            break;
        }
        items.push(Item {
            x,
            width: item_width,
            command,
            label,
        });
        x += item_width;
    }
    items
}
pub fn command_at(width: u16, x: u16) -> Option<&'static str> {
    items(width)
        .into_iter()
        .find(|item| x >= item.x && x < item.x + item.width)
        .map(|item| item.command)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_hits_only_complete_visible_items_at_every_width() {
        for width in 1..180 {
            for item in items(width) {
                assert!(item.x + item.width <= width);
                for x in item.x..item.x + item.width {
                    assert_eq!(command_at(width, x), Some(item.command));
                }
            }
            assert!(command_at(width, 0).is_none());
            assert!(command_at(width, width).is_none());
        }
    }
}
