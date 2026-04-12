use hymenium::parser::{parse_handoff, Dispatchability};

#[test]
fn test_crate_scaffold_handoff() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let result = parse_handoff(content);
    assert!(result.is_ok(), "Parse failed: {:?}", result.err());

    let handoff = result.unwrap();

    // Verify title
    assert_eq!(handoff.title, "Hymenium: Crate Scaffold");

    // Verify metadata was extracted
    assert!(handoff.metadata.is_some());
    let metadata = handoff.metadata.unwrap();
    assert_eq!(metadata.dispatchability, Dispatchability::Direct);
    assert_eq!(metadata.owning_repo, "hymenium");
    assert!(!metadata.allowed_write_scope.is_empty());
    assert!(metadata.allowed_write_scope[0].contains("hymenium"));

    // Verify problem section
    assert!(handoff.problem.len() > 0);
    assert!(handoff.problem.contains("Hymenium"));

    // Verify state items
    assert!(!handoff.state.is_empty());

    // Verify intent
    assert!(handoff.intent.len() > 0);

    // Verify steps were extracted
    assert!(!handoff.steps.is_empty());
    assert_eq!(handoff.steps.len(), 2);

    // Verify first step
    let step1 = &handoff.steps[0];
    assert_eq!(step1.number, 1);
    assert_eq!(step1.title, "Create Cargo.toml and module structure");
    assert_eq!(step1.project, Some("hymenium/".to_string()));
    assert_eq!(step1.effort, Some("2-3 hours".to_string()));

    // Verify verification block in first step
    assert!(step1.verification.is_some());
    let verify = step1.verification.as_ref().unwrap();
    assert!(!verify.commands.is_empty());

    // Verify checklist
    assert!(!step1.checklist.is_empty());

    // Verify completion protocol
    assert!(handoff.completion_protocol.is_some());
}

#[test]
fn test_sub_task_hierarchy_handoff() {
    let content = include_str!("fixtures/sub-task-hierarchy.md");
    let result = parse_handoff(content);
    assert!(result.is_ok(), "Parse failed: {:?}", result.err());

    let handoff = result.unwrap();

    // Verify title
    assert_eq!(handoff.title, "Canopy Sub-Task Hierarchy");

    // Verify metadata
    assert!(handoff.metadata.is_some());
    let metadata = handoff.metadata.unwrap();
    assert_eq!(metadata.dispatchability, Dispatchability::Direct);
    assert_eq!(metadata.owning_repo, "canopy");

    // Verify steps
    assert!(!handoff.steps.is_empty());
    assert_eq!(handoff.steps.len(), 3);

    // Verify step 2 has depends_on relationship
    let step1 = &handoff.steps[0];
    assert_eq!(step1.number, 1);
    assert_eq!(step1.title, "Enforce child-completion invariant at task completion");
    assert!(step1.effort.is_some());
    assert_eq!(step1.effort.as_ref().unwrap(), "1 day");

    // Verify step 2 can parallel with step 1
    let step2 = &handoff.steps[1];
    assert_eq!(step2.number, 2);
    assert_eq!(step2.title, "Add hierarchy rendering to task list");

    // Verify step 3 depends on step 2
    let step3 = &handoff.steps[2];
    assert_eq!(step3.number, 3);
    assert!(!step3.depends_on.is_empty());
    assert_eq!(step3.depends_on[0], "Step 2");

    // Verify files to modify were extracted
    assert!(!step1.files_to_modify.is_empty());
    let file = &step1.files_to_modify[0];
    assert!(file.path.contains("complete.rs"));

    // Verify context
    assert!(handoff.context.is_some());
}

#[test]
fn test_old_format_handoff_without_metadata() {
    let content = include_str!("fixtures/session-end-hook-old-format.md");
    let result = parse_handoff(content);
    assert!(result.is_ok(), "Parse failed: {:?}", result.err());

    let handoff = result.unwrap();

    // Verify title
    assert_eq!(handoff.title, "Session-End Hook for Automatic Memory Capture");

    // Verify metadata is None for old format
    assert!(handoff.metadata.is_none());

    // Verify problem section exists
    assert!(handoff.problem.len() > 0);

    // Verify state section
    assert!(!handoff.state.is_empty());

    // Verify intent
    assert!(handoff.intent.len() > 0);

    // Verify steps were extracted
    assert!(!handoff.steps.is_empty());
    assert_eq!(handoff.steps.len(), 3);

    // Verify step 1
    let step1 = &handoff.steps[0];
    assert_eq!(step1.number, 1);
    assert_eq!(step1.title, "Create session-summary hook script");

    // Verify checklist items in step 1
    assert!(!step1.checklist.is_empty());

    // Verify step 2 depends on step 1
    let step2 = &handoff.steps[1];
    assert_eq!(step2.number, 2);
    assert!(!step2.depends_on.is_empty());
    assert_eq!(step2.depends_on[0], "Step 1");

    // Verify step 3 depends on step 1
    let step3 = &handoff.steps[2];
    assert_eq!(step3.number, 3);
    assert!(!step3.depends_on.is_empty());
}

#[test]
fn test_step_effort_estimates() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let handoff = parse_handoff(content).unwrap();

    for step in &handoff.steps {
        // All steps in this handoff should have effort estimates
        assert!(step.effort.is_some(), "Step {} missing effort", step.number);
    }
}

#[test]
fn test_verification_commands_extracted() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let handoff = parse_handoff(content).unwrap();

    for step in &handoff.steps {
        // All steps should have verification blocks
        assert!(step.verification.is_some(), "Step {} missing verification", step.number);
        let verify = step.verification.as_ref().unwrap();
        assert!(!verify.commands.is_empty(), "Step {} has no commands", step.number);
    }
}

#[test]
fn test_completion_protocol_present() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let handoff = parse_handoff(content).unwrap();

    assert!(handoff.completion_protocol.is_some());
    let protocol = handoff.completion_protocol.unwrap();
    assert!(protocol.contains("complete"));
}

#[test]
fn test_context_section_extracted() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let handoff = parse_handoff(content).unwrap();

    assert!(handoff.context.is_some());
    let context = handoff.context.unwrap();
    assert!(context.len() > 0);
    assert!(context.contains("#118"));
}

#[test]
fn test_checklist_items_with_status() {
    let content = include_str!("fixtures/crate-scaffold.md");
    let handoff = parse_handoff(content).unwrap();

    let step = &handoff.steps[0];
    assert!(!step.checklist.is_empty());

    // Check that we parse both checked and unchecked items
    let mut has_unchecked = false;

    for item in &step.checklist {
        if !item.checked {
            has_unchecked = true;
            break;
        }
    }

    assert!(has_unchecked, "Step should have unchecked items");
}

#[test]
fn test_project_field_optional() {
    let content = include_str!("fixtures/sub-task-hierarchy.md");
    let handoff = parse_handoff(content).unwrap();

    for step in &handoff.steps {
        // Some steps may have project field, some may not
        if let Some(ref project) = step.project {
            assert!(!project.is_empty());
        }
    }
}

#[test]
fn test_missing_problem_section_fails() {
    let bad_content = "# Title\n\n## Intent\n\nSome intent";
    let result = parse_handoff(bad_content);
    assert!(result.is_err());
}

#[test]
fn test_missing_steps_fails() {
    let bad_content = "# Title\n\n## Problem\n\nProblem\n\n## What needs doing (intent)\n\nIntent";
    let result = parse_handoff(bad_content);
    assert!(result.is_err());
}
