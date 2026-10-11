// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validated nested package identifiers (AOUSD Core §9.7, §16.4).
use super::*;

/// An outer identifier followed by zero or more normalized package members.
/// Brackets must nest, each member stays inside its containing package, and
/// depth is bounded before allocation. The outer identifier keeps host policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagePath {
    parts: Vec<String>,
}
impl PackagePath {
    /// Parses `outer.usdz[inner.usdz[layer.usda]]`, with an explicit maximum
    /// number of member levels. Malformed brackets and member escapes fail.
    pub fn parse(identifier: &str, max_depth: usize) -> Result<Self, &'static str> {
        let mut parts = Vec::new();
        let mut remaining = identifier;
        loop {
            let (part, rest) = match remaining.find('[') {
                Some(start) => {
                    if !remaining.ends_with(']') {
                        return Err("unbalanced package brackets");
                    }
                    (
                        &remaining[..start],
                        Some(&remaining[start + 1..remaining.len() - 1]),
                    )
                }
                None => (remaining, None),
            };
            if part.is_empty() || part.contains(']') {
                return Err("empty or malformed package path");
            }
            let part = if parts.is_empty() {
                String::from(part)
            } else {
                let path = normalize_asset_path(&part.replace('\\', "/"));
                let path = path.strip_prefix("./").unwrap_or(&path);
                if path.is_empty()
                    || path == ".."
                    || path.starts_with("../")
                    || path.starts_with('/')
                    || path.contains([':', '\0'])
                {
                    return Err("package member escapes its containing package");
                }
                String::from(path)
            };
            parts.push(part);
            match rest {
                Some(rest) if parts.len() <= max_depth => remaining = rest,
                Some(_) => return Err("package nesting exceeds depth budget"),
                None => return Ok(Self { parts }),
            }
        }
    }
    /// Host-owned outer source identifier.
    pub fn outer(&self) -> &str {
        &self.parts[0]
    }
    /// Normalized members in outer-to-inner order.
    pub fn members(&self) -> &[String] {
        &self.parts[1..]
    }
    /// Innermost selected name, or the outer identifier for a loose source.
    pub fn leaf(&self) -> &str {
        self.parts.last().expect("nonempty identifier")
    }
    /// Replaces only the host identifier, retaining validated member paths.
    pub fn with_outer(&self, outer: &str) -> String {
        Self::join(outer, self.members())
    }
    /// Formats validated member levels beneath an outer identifier.
    pub fn join(outer: &str, members: &[String]) -> String {
        let mut result = String::from(outer);
        for member in members {
            result.push('[');
            result.push_str(member);
        }
        for _ in members {
            result.push(']');
        }
        result
    }
    /// Anchors a relative asset to the innermost member's directory, retaining
    /// the containing package chain. `None` on escape or malformed input.
    pub fn anchor_member(asset: &str, authoring: &str) -> Option<String> {
        let authoring = Self::parse(authoring, 32).ok()?;
        let asset = Self::parse(asset, 32).ok()?;
        if asset.outer().starts_with('/') || asset.outer().contains(':') {
            return None;
        }
        let directory = authoring.leaf().rsplit_once('/').map_or("", |(d, _)| d);
        let joined = if directory.is_empty() {
            String::from(asset.outer())
        } else {
            alloc::format!("{directory}/{}", asset.outer())
        };
        let selected = normalize_asset_path(&joined);
        // Use a synthetic outer so validation also applies to the first member.
        let selected = selected.strip_prefix("./").unwrap_or(&selected);
        let mut members = authoring.parts;
        members.pop();
        members.push(String::from(selected));
        members.extend_from_slice(asset.members());
        let result = Self::join(&members[0], &members[1..]);
        Self::parse(&alloc::format!("package[{result}]"), 32).ok()?;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_every_level_and_anchors_inside_the_innermost_package() {
        let path = PackagePath::parse("a.usdz[b.usdz[dir/../c.usda]]", 2).unwrap();
        assert_eq!(
            path.with_outer("/root/a.usdz"),
            "/root/a.usdz[b.usdz[c.usda]]"
        );
        assert!(PackagePath::parse("a[b[c]]", 1).is_err());
        for path in [
            "a[b][c]",
            "a[b",
            "a[]",
            "a[../b]",
            "a[b[/c]]",
            "a[b[c]]]",
            "a[b[c:foo]]",
            "a[b\0c]",
        ] {
            assert!(PackagePath::parse(path, 32).is_err(), "{path}");
        }
        assert_eq!(
            PackagePath::anchor_member("../next.usda", "inner.usdz[dir/root.usda]").as_deref(),
            Some("inner.usdz[next.usda]")
        );
        assert!(
            PackagePath::anchor_member("../../outside.usda", "inner.usdz[dir/root.usda]").is_none()
        );
    }
}
