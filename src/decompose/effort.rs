//! Effort estimation and agent tier assignment.

use crate::workflow::template::AgentTier;

use super::DecompositionError;

/// Parse an effort string like "2-3 hours", "1 day", "30 minutes", "4h" into
/// seconds. For ranges, the maximum value is used.
pub fn parse_effort_secs(effort: &str) -> Result<u64, DecompositionError> {
    let s = effort.trim().to_lowercase();

    // Detect the unit.
    let (numeric_part, multiplier) = if s.contains("day") {
        let num = s
            .split(|c: char| c.is_alphabetic() || c == ' ')
            .next()
            .unwrap_or("");
        (num, 8 * 3600u64) // 1 day = 8 working hours
    } else if s.contains("hour") || s.ends_with('h') {
        let num = s
            .split(|c: char| c.is_alphabetic() || c == ' ')
            .next()
            .unwrap_or("");
        (num, 3600u64)
    } else if s.contains("minute") || s.ends_with('m') {
        let num = s
            .split(|c: char| c.is_alphabetic() || c == ' ')
            .next()
            .unwrap_or("");
        (num, 60u64)
    } else {
        return Err(DecompositionError::InvalidEffort(effort.to_string()));
    };

    // Reject leading minus sign before attempting range or single-value parse.
    if numeric_part.starts_with('-') {
        return Err(DecompositionError::InvalidEffort(effort.to_string()));
    }

    // The numeric part may be a range like "2-3" or a single number like "4".
    let value = if numeric_part.contains('-') {
        // All components of the range must parse — reject partial ranges like "1-x".
        let parts: Result<Vec<f64>, _> = numeric_part
            .split('-')
            .map(|p| p.trim().parse::<f64>())
            .collect();
        let parts = parts.map_err(|_| DecompositionError::InvalidEffort(effort.to_string()))?;
        parts.into_iter().fold(0.0f64, f64::max)
    } else {
        numeric_part
            .trim()
            .parse::<f64>()
            .map_err(|_| DecompositionError::InvalidEffort(effort.to_string()))?
    };

    if value <= 0.0 {
        return Err(DecompositionError::InvalidEffort(effort.to_string()));
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let secs = (value * multiplier as f64) as u64;
    Ok(secs)
}

/// Assign an agent tier based on effort in seconds.
pub(super) fn tier_from_effort_secs(secs: u64) -> AgentTier {
    #[allow(clippy::cast_precision_loss)]
    let hours = secs as f64 / 3600.0;
    if hours > 6.0 {
        AgentTier::Opus
    } else if hours >= 2.0 {
        AgentTier::Sonnet
    } else {
        AgentTier::Haiku
    }
}

/// Assign an agent tier based on step count alone.
pub(super) fn tier_from_step_count(count: usize) -> AgentTier {
    match count {
        0..=1 => AgentTier::Haiku,
        2..=3 => AgentTier::Sonnet,
        _ => AgentTier::Opus,
    }
}
