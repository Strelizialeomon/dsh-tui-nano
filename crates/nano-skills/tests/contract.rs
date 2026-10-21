//! S05 contract conformance: the fixtures in `protocol/nano-protocol.json`
//! must parse with the Rust projections, and the preview limit must match.

use nano_skills::{
    InstructionState, NameResolution, SkillCatalog, SkillPreview, MAX_PREVIEW_BYTES,
};
use serde_json::Value;

const CONTRACT_TEXT: &str = include_str!("../../../protocol/nano-protocol.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_TEXT).expect("protocol contract is valid JSON")
}

#[test]
fn preview_limit_matches_the_contract() {
    let contract = contract();
    assert_eq!(
        contract["limits"]["maxSkillPreviewBytes"].as_u64(),
        Some(MAX_PREVIEW_BYTES)
    );
}

#[test]
fn skill_list_fixture_folds_into_the_catalog() {
    let contract = contract();
    let result = &contract["fixtures"]["skillsListResult"]["result"];
    let mut catalog = SkillCatalog::new();
    catalog.apply_list(result, "session-fixture").unwrap();
    assert!(catalog.complete());
    assert_eq!(catalog.entries().len(), 1);
    let entry = &catalog.entries()[0];
    assert_eq!(entry.name, "hello-skill");
    assert_eq!(entry.when_to_use.as_deref(), Some("When testing"));
    assert!(entry.user_only());
    assert_eq!(
        entry.path.as_deref(),
        Some("/tmp/skills/hello-skill/SKILL.md")
    );
    assert_eq!(catalog.resolve("hello-skill"), NameResolution::Skill);
    assert_eq!(catalog.resolve("compact"), NameResolution::Command);
    assert_eq!(catalog.resolve("missing"), NameResolution::Unknown);
}

#[test]
fn preview_fixture_parses_and_exposes_the_limit() {
    let contract = contract();
    let result = &contract["fixtures"]["skillPreviewResult"]["result"];
    let preview = SkillPreview::parse(result, "session-fixture").unwrap();
    assert_eq!(preview.name, "hello-skill");
    assert_eq!(preview.text, "# hello-skill\n");
    assert!(!preview.truncated);
    assert_eq!(preview.limit, MAX_PREVIEW_BYTES);
    assert!(preview.truncation_label().is_none());
}

#[test]
fn instruction_event_fixture_folds_into_the_source_projection() {
    let contract = contract();
    let payload = &contract["fixtures"]["instructionsEvent"]["params"]["payload"];
    let mut instructions = InstructionState::new();
    instructions.apply(payload, "session-fixture").unwrap();
    assert!(instructions.available);
    assert!(instructions.baseline);
    assert_eq!(instructions.loaded_at, Some(1_792_331_608_209));
    assert_eq!(instructions.sources.len(), 1);
    assert_eq!(instructions.sources[0].path, "AGENTS.md");
    assert!(instructions
        .budget_notice
        .as_deref()
        .unwrap()
        .starts_with("Workspace instruction budget"));
}

#[test]
fn the_method_and_event_owners_are_s05() {
    let contract = contract();
    for name in [
        "nano.skills.list",
        "nano.skills.preview",
        "nano.instructions.snapshot",
    ] {
        let method = contract["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|method| method["name"] == name)
            .unwrap_or_else(|| panic!("{name} is in the contract"));
        assert_eq!(method["owner"], "S05");
        assert_eq!(method["status"], "implemented");
    }
    for kind in ["skills.changed", "instructions.changed"] {
        let event = contract["eventKinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == kind)
            .unwrap_or_else(|| panic!("{kind} is in the contract"));
        assert_eq!(event["owner"], "S05");
        assert_eq!(event["status"], "implemented");
    }
}
