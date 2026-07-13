//! Pure peer resolution: turn a `--to` query into a single device, matching by
//! exact id, then exact name, then unique name prefix. Ambiguity is reported
//! so the caller can list candidates rather than guess.

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact(usize),
    Ambiguous(Vec<usize>),
    NotFound,
}

/// `candidates` is `(id, name)` per device.
pub fn resolve(candidates: &[(String, String)], query: &str) -> Resolution {
    // 1. Exact id.
    if let Some(i) = candidates.iter().position(|(id, _)| id == query) {
        return Resolution::Exact(i);
    }
    let q = query.to_ascii_lowercase();

    // 2. Exact name (case-insensitive).
    let name_exact: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, name))| name.to_ascii_lowercase() == q)
        .map(|(i, _)| i)
        .collect();
    match name_exact.len() {
        1 => return Resolution::Exact(name_exact[0]),
        n if n > 1 => return Resolution::Ambiguous(name_exact),
        _ => {}
    }

    // 3. Name prefix.
    let prefix: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, name))| name.to_ascii_lowercase().starts_with(&q))
        .map(|(i, _)| i)
        .collect();
    match prefix.len() {
        1 => Resolution::Exact(prefix[0]),
        0 => Resolution::NotFound,
        _ => Resolution::Ambiguous(prefix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> Vec<(String, String)> {
        vec![
            ("id-1".into(), "Alice MacBook".into()),
            ("id-2".into(), "Alice Phone".into()),
            ("id-3".into(), "home-server".into()),
        ]
    }

    #[test]
    fn exact_id_wins() {
        assert_eq!(resolve(&cands(), "id-2"), Resolution::Exact(1));
    }

    #[test]
    fn exact_name_case_insensitive() {
        assert_eq!(resolve(&cands(), "home-SERVER"), Resolution::Exact(2));
    }

    #[test]
    fn unique_prefix() {
        assert_eq!(resolve(&cands(), "home"), Resolution::Exact(2));
    }

    #[test]
    fn ambiguous_prefix_lists_all() {
        assert_eq!(
            resolve(&cands(), "alice"),
            Resolution::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn not_found() {
        assert_eq!(resolve(&cands(), "nope"), Resolution::NotFound);
    }
}
