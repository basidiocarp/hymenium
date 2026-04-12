//! Markdown parsing for handoff documents.
//!
//! Parses markdown documents to extract structured handoff information,
//! including metadata, steps, and verification blocks.

use crate::parser::{
    ChecklistItem, Dispatchability, FileModification, HandoffMetadata, ParseError, ParsedHandoff,
    ParsedStep, PasteMarker, VerificationBlock,
};

/// Parse a handoff markdown document into structured data.
#[allow(clippy::similar_names)] // content (input) vs context (section) are clear in usage
pub fn parse_handoff(content: &str) -> Result<ParsedHandoff, ParseError> {
    let lines: Vec<&str> = content.lines().collect();

    let title = extract_title(&lines)?;
    let metadata = extract_metadata(&lines);
    let problem = extract_section(&lines, "## Problem")?;
    let state = extract_list_section(&lines, "## What exists (state)");
    let intent = extract_section(&lines, "## What needs doing (intent)")?;
    let steps = extract_steps(&lines)?;
    let completion_protocol = extract_section(&lines, "## Completion Protocol").ok();
    let context = extract_section(&lines, "## Context").ok();

    Ok(ParsedHandoff {
        title,
        metadata,
        problem,
        state,
        intent,
        steps,
        completion_protocol,
        context,
    })
}

fn extract_title(lines: &[&str]) -> Result<String, ParseError> {
    for line in lines {
        if let Some(title) = line.strip_prefix("# ") {
            return Ok(title.trim().to_string());
        }
    }
    Err(ParseError::MissingSection("title (# ...)".to_string()))
}

fn extract_metadata(lines: &[&str]) -> Option<HandoffMetadata> {
    let mut in_metadata = false;
    let mut metadata_lines = Vec::new();

    for line in lines {
        if *line == "## Handoff Metadata" {
            in_metadata = true;
            continue;
        }
        if in_metadata {
            if line.starts_with("## ") {
                break;
            }
            if !line.is_empty() {
                metadata_lines.push(line);
            }
        }
    }

    if metadata_lines.is_empty() {
        return None;
    }

    let mut metadata = HandoffMetadata {
        dispatchability: Dispatchability::Direct,
        owning_repo: String::new(),
        allowed_write_scope: Vec::new(),
        cross_repo_rule: None,
        non_goals: Vec::new(),
        verification_contract: String::new(),
        completion_update: String::new(),
    };

    for line in metadata_lines {
        if let Some(value) = parse_metadata_line(line) {
            match value.0.as_str() {
                "Dispatch" => {
                    metadata.dispatchability = if value.1.contains("umbrella") {
                        Dispatchability::Umbrella
                    } else {
                        Dispatchability::Direct
                    };
                }
                "Owning repo" => {
                    metadata.owning_repo = value.1.trim_matches('`').to_string();
                }
                "Allowed write scope" => {
                    metadata.allowed_write_scope = split_scope(&value.1)
                        .into_iter()
                        .map(std::string::ToString::to_string)
                        .collect();
                }
                "Cross-repo edits" => {
                    if !value.1.to_lowercase().contains("none") {
                        metadata.cross_repo_rule = Some(value.1);
                    }
                }
                "Non-goals" => {
                    metadata.non_goals = split_list(&value.1);
                }
                "Verification contract" => {
                    metadata.verification_contract = value.1;
                }
                "Completion update" => {
                    metadata.completion_update = value.1;
                }
                _ => {}
            }
        }
    }

    Some(metadata)
}

fn parse_metadata_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("- **")?;
    if let Some(colon_pos) = rest.find(":**") {
        let key = rest[..colon_pos].to_string();
        let value = rest[colon_pos + 3..].trim().to_string();
        Some((key, value))
    } else {
        None
    }
}

fn split_scope(s: &str) -> Vec<&str> {
    s.split(',').map(str::trim).collect()
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn extract_section(lines: &[&str], section_heading: &str) -> Result<String, ParseError> {
    let mut result = Vec::new();
    let mut in_section = false;
    let mut in_code_block = false;

    for line in lines {
        if *line == section_heading {
            in_section = true;
            continue;
        }

        if in_section {
            // Stop at next section heading or horizontal rule
            if !in_code_block
                && (line.starts_with("## ") || line.starts_with("### ") || line.starts_with("---"))
            {
                break;
            }

            if line.starts_with("```") {
                in_code_block = !in_code_block;
                result.push(line.to_string());
            } else if in_code_block || !line.is_empty() {
                result.push(line.to_string());
            }
        }
    }

    let text = result.join("\n").trim().to_string();
    if text.is_empty() {
        Err(ParseError::MissingSection(section_heading.to_string()))
    } else {
        Ok(text)
    }
}

fn extract_list_section(lines: &[&str], section_heading: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut in_section = false;

    for line in lines {
        if *line == section_heading {
            in_section = true;
            continue;
        }

        if in_section {
            if line.starts_with("## ") {
                break;
            }

            // Capture the full "key: value" from list items.
            // Handles both "- **key:** value" and "- **key**: value" formats.
            if let Some(rest) = line.strip_prefix("- **") {
                // Try "**:" format first (e.g., "- **hymenium/**: description")
                if let Some(pos) = rest.find("**:") {
                    let key = rest[..pos].to_string();
                    let value = rest[pos + 3..].trim().to_string();
                    if value.is_empty() {
                        result.push(key);
                    } else {
                        result.push(format!("{}: {}", key, value));
                    }
                }
                // Try ":**" format (e.g., "- **Dispatch:** `direct`")
                else if let Some(pos) = rest.find(":**") {
                    let key = rest[..pos].to_string();
                    let value = rest[pos + 3..].trim().to_string();
                    if value.is_empty() {
                        result.push(key);
                    } else {
                        result.push(format!("{}: {}", key, value));
                    }
                }
            }
        }
    }

    result
}

fn extract_steps(lines: &[&str]) -> Result<Vec<ParsedStep>, ParseError> {
    let mut steps = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("### Step ") && line.contains(':') {
            if let Some(step_num) = extract_step_number(line) {
                let title = extract_step_title(line);
                let step_end = find_next_section_boundary(lines, idx);

                let step_lines = &lines[idx..step_end];
                let step = parse_step(step_num, title, step_lines);
                steps.push(step);
            }
        }
    }

    if steps.is_empty() {
        return Err(ParseError::MissingSection("### Step N: ...".to_string()));
    }

    Ok(steps)
}

fn extract_step_number(line: &str) -> Option<u32> {
    if let Some(start) = line.find("Step ") {
        let rest = &line[start + 5..];
        let num_str: String = rest.chars().take_while(char::is_ascii_digit).collect();
        num_str.parse::<u32>().ok()
    } else {
        None
    }
}

fn extract_step_title(line: &str) -> String {
    if let Some(colon_pos) = line.find(':') {
        line[colon_pos + 1..].trim().to_string()
    } else {
        String::new()
    }
}

fn find_next_section_boundary(lines: &[&str], from_idx: usize) -> usize {
    for (idx, line) in lines.iter().enumerate().skip(from_idx + 1) {
        if (line.starts_with("### ") && idx > from_idx)
            || line.starts_with("## ")
            || line.starts_with("---")
        {
            return idx;
        }
    }
    lines.len()
}

fn parse_step(number: u32, title: String, lines: &[&str]) -> ParsedStep {
    let mut project = None;
    let mut effort = None;
    let mut depends_on = Vec::new();
    let mut files_to_modify = Vec::new();
    let mut verification = None;
    let mut checklist = Vec::new();

    let mut in_metadata = true;
    let mut in_description = false;
    let mut in_files = false;
    let mut in_verification = false;
    let mut in_checklist = false;
    let mut desc_lines = Vec::new();
    let mut verify_lines = Vec::new();

    for line in lines.iter().skip(1) {
        if in_metadata && line.starts_with("**Project:**") {
            project =
                extract_key_value(line, "**Project:**").map(|v| v.trim_matches('`').to_string());
        } else if in_metadata && line.starts_with("**Effort:**") {
            effort = extract_key_value(line, "**Effort:**");
        } else if in_metadata && line.starts_with("**Depends on:**") {
            depends_on = extract_dependencies(line);
        } else if line.starts_with("#### Files to modify") {
            in_metadata = false;
            in_description = false;
            in_files = true;
            in_verification = false;
            in_checklist = false;
        } else if line.starts_with("#### Verification") {
            in_metadata = false;
            in_description = false;
            in_files = false;
            in_verification = true;
            in_checklist = false;
        } else if line.starts_with("**Checklist:**") || line.starts_with("**Checklist :**") {
            in_metadata = false;
            in_description = false;
            in_files = false;
            in_verification = false;
            in_checklist = true;
        } else if line.starts_with("---") || (line.starts_with("### ") && !line.contains(':')) {
            break;
        } else if in_verification && (line.starts_with("```") || !verify_lines.is_empty()) {
            verify_lines.push(line.to_string());
        } else if in_checklist && line.starts_with("- [") {
            let checked = line.contains("[x]") || line.contains("[X]");
            let text = line[4..].trim_start_matches(']').trim().to_string();
            checklist.push(ChecklistItem { text, checked });
        } else if in_files && line.starts_with("**`") {
            if let Some((path, desc)) = extract_file_modification(line) {
                files_to_modify.push(FileModification {
                    path,
                    description: desc,
                });
            }
        } else if in_metadata && !line.starts_with("**") && !line.is_empty() {
            in_metadata = false;
            in_description = true;
            desc_lines.push(line.to_string());
        } else if in_description && !line.starts_with("####") {
            desc_lines.push(line.to_string());
        }
    }

    let description = desc_lines.join("\n").trim().to_string();

    if !verify_lines.is_empty() {
        verification = Some(parse_verification_block(&verify_lines));
    }

    ParsedStep {
        number,
        title,
        project,
        effort,
        depends_on,
        description,
        files_to_modify,
        verification,
        checklist,
    }
}

fn extract_key_value(line: &str, prefix: &str) -> Option<String> {
    if let Some(start) = line.find(prefix) {
        let rest = line[start + prefix.len()..].trim();
        Some(rest.to_string())
    } else {
        None
    }
}

fn extract_dependencies(line: &str) -> Vec<String> {
    if let Some(start) = line.find("**Depends on:**") {
        let rest = &line[start + 15..];
        rest.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}

fn extract_file_modification(line: &str) -> Option<(String, String)> {
    let end_backtick = line.find("`** ")?;
    let path = line[3..end_backtick].to_string();
    let rest = &line[end_backtick + 4..];

    let description = if let Some(sep) = rest.find('\u{2014}') {
        // em-dash
        rest[sep..]
            .trim_start_matches('\u{2014}')
            .trim_start()
            .to_string()
    } else if let Some(sep) = rest.find('-') {
        rest[sep..].trim_start_matches('-').trim_start().to_string()
    } else {
        rest.trim().to_string()
    };

    Some((path, description))
}

fn parse_verification_block(lines: &[String]) -> VerificationBlock {
    let mut commands = Vec::new();
    let mut paste_markers = Vec::new();
    let mut in_code_block = false;
    let mut current_cmd = Vec::new();
    let mut in_paste_region = false;
    let mut paste_has_content = false;

    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                current_cmd.clear();
            } else if !current_cmd.is_empty() {
                commands.push(current_cmd.join("\n"));
                current_cmd.clear();
            }
        } else if in_code_block {
            current_cmd.push(line.clone());
        } else if line.contains("<!-- PASTE START -->") {
            in_paste_region = true;
            paste_has_content = false;
            paste_markers.push(PasteMarker {
                line_number: idx,
                has_content: false, // will be updated when we see PASTE END
            });
        } else if line.contains("<!-- PASTE END -->") {
            if in_paste_region {
                // Update the last PASTE START marker with content status
                if let Some(start_marker) = paste_markers.last_mut() {
                    start_marker.has_content = paste_has_content;
                }
            }
            paste_markers.push(PasteMarker {
                line_number: idx,
                has_content: paste_has_content,
            });
            in_paste_region = false;
        } else if in_paste_region && !line.trim().is_empty() {
            paste_has_content = true;
        }
    }

    if !current_cmd.is_empty() {
        commands.push(current_cmd.join("\n"));
    }

    VerificationBlock {
        commands,
        paste_markers,
    }
}
