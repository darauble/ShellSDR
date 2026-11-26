//! MID -> administration, from the ITU table (seed spec/03-domain/mid.csv).

use std::collections::HashMap;
use std::sync::OnceLock;

/// The administration(s) of a MID, several joined by " / " (PAR-07).
pub fn administrations(mid: u16) -> Option<String> {
    table().get(&mid).cloned()
}

fn table() -> &'static HashMap<u16, String> {
    static TABLE: OnceLock<HashMap<u16, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t: HashMap<u16, String> = HashMap::new();
        // Header `mid,allocated_to`; one row per territory, in table order.
        for row in crate::dsc::csv::parse(include_str!("../data/mid.csv"))
            .into_iter()
            .skip(1)
        {
            let (Some(mid), Some(name)) = (row.first().and_then(|m| m.parse().ok()), row.get(1))
            else {
                continue;
            };
            t.entry(mid)
                .and_modify(|all| {
                    all.push_str(" / ");
                    all.push_str(name);
                })
                .or_insert_with(|| name.clone());
        }
        t
    })
}
