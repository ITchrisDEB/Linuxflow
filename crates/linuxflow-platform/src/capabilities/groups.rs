//! Pure reading of `/proc/self/status` and `/etc/group` (membership of the `input` group).

/// Gids listed by `/proc/self/status` (`Gid:` and `Groups:` lines).
pub fn parse_status_gids(status: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line
            .strip_prefix("Gid:")
            .or_else(|| line.strip_prefix("Groups:"))
        {
            out.extend(
                rest.split_whitespace()
                    .filter_map(|w| w.parse::<u32>().ok()),
            );
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// (gid, name) pairs of `/etc/group`.
pub fn parse_group_file(content: &str) -> Vec<(u32, String)> {
    content
        .lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let _pw = f.next()?;
            let gid = f.next()?.parse().ok()?;
            (!name.is_empty()).then(|| (gid, name.to_string()))
        })
        .collect()
}

pub fn group_names_for(gids: &[u32], groups: &[(u32, String)]) -> Vec<String> {
    groups
        .iter()
        .filter(|(g, _)| gids.contains(g))
        .map(|(_, n)| n.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gids_and_names() {
        let status = "Name:\tx\nGid:\t1000\t1000\t1000\t1000\nGroups:\t10 105 1000\n";
        let gids = parse_status_gids(status);
        assert_eq!(gids, vec![10, 105, 1000]);
        let groups = parse_group_file("root:x:0:\ninput:x:105:sam\nwheel:x:10:sam\n# bad\n:x:5:\n");
        assert_eq!(
            group_names_for(&gids, &groups),
            vec!["input".to_string(), "wheel".to_string()]
        );
    }

    #[test]
    fn entrees_invalides_ignorees() {
        assert!(parse_status_gids("Groups:\tabc\n").is_empty());
        assert!(parse_group_file("n:x:notanumber:\n").is_empty());
    }
}
