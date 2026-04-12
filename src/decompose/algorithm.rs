//! Core decomposition algorithms and helpers.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::parser::ParsedStep;

use super::{effort, DecompositionConfig};

/// Group steps by their project field. Steps with no project go into "default".
pub(super) fn group_by_project(
    steps: &[ParsedStep],
    respect_boundaries: bool,
) -> Vec<(String, Vec<&ParsedStep>)> {
    if !respect_boundaries {
        return vec![("default".to_string(), steps.iter().collect())];
    }

    let mut groups: BTreeMap<String, Vec<&ParsedStep>> = BTreeMap::new();
    for step in steps {
        let key = step.project.as_deref().unwrap_or("default").to_string();
        groups.entry(key).or_default().push(step);
    }
    groups.into_iter().collect()
}

/// Parse a dependency string like "Step 1" or "step 3" into the step number.
pub(super) fn parse_dep_step_number(dep: &str) -> Option<u32> {
    let lower = dep.trim().to_lowercase();
    let num_str = lower.strip_prefix("step")?;
    num_str.trim().parse::<u32>().ok()
}

/// Build a mapping from step number to which other step numbers it depends on.
pub(super) fn build_step_deps(steps: &[&ParsedStep]) -> HashMap<u32, Vec<u32>> {
    let mut map = HashMap::new();
    for step in steps {
        let deps: Vec<u32> = step
            .depends_on
            .iter()
            .filter_map(|d| parse_dep_step_number(d))
            .collect();
        map.insert(step.number, deps);
    }
    map
}

/// Given a set of steps and their dependencies, merge steps into connected
/// components so that dependent steps always stay together.
pub(super) fn merge_dependency_groups<'a>(steps: &[&'a ParsedStep]) -> Vec<Vec<&'a ParsedStep>> {
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let p = parent[&x];
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }

    fn union(parent: &mut HashMap<u32, u32>, a: u32, b: u32) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }

    if steps.is_empty() {
        return Vec::new();
    }

    let step_numbers: HashSet<u32> = steps.iter().map(|s| s.number).collect();
    let deps = build_step_deps(steps);

    // Union-Find via parent map.
    let mut parent: HashMap<u32, u32> = HashMap::new();
    for &n in &step_numbers {
        parent.insert(n, n);
    }

    for (&step_num, dep_list) in &deps {
        for &dep in dep_list {
            if step_numbers.contains(&dep) {
                union(&mut parent, step_num, dep);
            }
        }
    }

    // Group steps by their root.
    let mut groups: BTreeMap<u32, Vec<&ParsedStep>> = BTreeMap::new();
    for step in steps {
        let root = find(&mut parent, step.number);
        groups.entry(root).or_default().push(step);
    }

    // Sort each group by step number for determinism.
    let mut result: Vec<Vec<&ParsedStep>> = groups.into_values().collect();
    for group in &mut result {
        group.sort_by_key(|s| s.number);
    }
    result.sort_by_key(|g| g.first().map_or(0, |s| s.number));
    result
}

/// Pack atomic chunks (dependency groups) into pieces, respecting effort or
/// step-count limits. Each chunk is indivisible — it always stays together.
pub(super) fn pack_chunks<'a>(
    chunks: &[Vec<&'a ParsedStep>],
    config: &DecompositionConfig,
    warnings: &mut Vec<String>,
) -> Vec<Vec<&'a ParsedStep>> {
    if chunks.is_empty() {
        return Vec::new();
    }

    // Determine whether to use effort-based or count-based packing.
    let all_steps: Vec<&&ParsedStep> = chunks.iter().flat_map(|c| c.iter()).collect();
    let has_effort = all_steps.iter().any(|s| s.effort.is_some());

    let mut pieces: Vec<Vec<&'a ParsedStep>> = Vec::new();
    let mut current: Vec<&'a ParsedStep> = Vec::new();

    if has_effort {
        let mut current_secs: u64 = 0;

        for chunk in chunks {
            let chunk_secs = chunk_effort_secs(chunk, warnings);

            if !current.is_empty() && current_secs + chunk_secs > config.max_effort_per_piece_secs {
                pieces.push(std::mem::take(&mut current));
                current_secs = 0;
            }
            current.extend_from_slice(chunk);
            current_secs += chunk_secs;
        }
    } else {
        let mut current_count: usize = 0;

        for chunk in chunks {
            if !current.is_empty() && current_count + chunk.len() > config.max_steps_per_piece {
                pieces.push(std::mem::take(&mut current));
                current_count = 0;
            }
            current.extend_from_slice(chunk);
            current_count += chunk.len();
        }
    }

    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// Compute the total effort in seconds for a chunk, logging warnings for
/// unparseable effort strings.
pub(super) fn chunk_effort_secs(steps: &[&ParsedStep], warnings: &mut Vec<String>) -> u64 {
    let mut total: u64 = 0;
    for step in steps {
        if let Some(ref effort) = step.effort {
            match effort::parse_effort_secs(effort) {
                Ok(secs) => total += secs,
                Err(err) => {
                    warnings.push(format!("step {}: {}", step.number, err));
                }
            }
        }
    }
    total
}

/// Compute the total effort in seconds for a set of steps, or None if no effort
/// estimates were successfully parsed.
pub(super) fn total_effort_secs(steps: &[&ParsedStep], warnings: &mut Vec<String>) -> Option<u64> {
    let mut total: u64 = 0;
    let mut any_parsed = false;

    for step in steps {
        if let Some(ref effort) = step.effort {
            match effort::parse_effort_secs(effort) {
                Ok(secs) => {
                    total += secs;
                    any_parsed = true;
                }
                Err(err) => {
                    warnings.push(format!("step {}: {}", step.number, err));
                }
            }
        }
    }

    if any_parsed {
        Some(total)
    } else {
        None
    }
}

/// Generate a human-readable title for a set of steps.
pub(super) fn piece_title(steps: &[&ParsedStep]) -> String {
    match steps.len() {
        0 => String::new(),
        1 => steps[0].title.clone(),
        _ => {
            let first = steps.first().expect("checked non-empty");
            let last = steps.last().expect("checked non-empty");
            format!(
                "Steps {}-{}: {} & {}",
                first.number, last.number, first.title, last.title
            )
        }
    }
}

/// Convert text to a URL-safe slug.
pub(super) fn slugify(text: &str) -> String {
    let raw: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    raw.split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
