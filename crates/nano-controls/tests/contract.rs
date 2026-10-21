//! Contract fixtures for S04: the shared `controlsState` and `controlsCatalog`
//! samples in `protocol/nano-protocol.json` must parse into the display model.
//! This is what keeps the `sandbox`/`approval` fields and catalog grouping from
//! drifting away from the machine-readable contract.

use serde_json::Value;

use nano_controls::{ControlsModel, ModelCatalog};

const CONTRACT_TEXT: &str = include_str!("../../../protocol/nano-protocol.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_TEXT).expect("protocol contract is valid JSON")
}

#[test]
fn controls_state_fixture_parses_into_the_display_model() {
    let contract = contract();
    let result = &contract["fixtures"]["controlsState"]["result"];
    let mut controls = ControlsModel::new();
    controls.set_capabilities(true, true, true);
    controls.apply_state(result).expect("fixture parses");

    assert_eq!(
        controls
            .model
            .next
            .as_ref()
            .unwrap()
            .reasoning_effort
            .as_deref(),
        Some("high")
    );
    assert_eq!(controls.permission_label(), "workspace-write");
    let danger = controls
        .permission()
        .catalog
        .find("danger-full-access")
        .expect("preset fixture kept");
    assert_eq!(danger.sandbox.as_deref(), Some("danger-full-access"));
    assert_eq!(danger.approval.as_deref(), Some("never"));
    assert!(danger.approval_never());
    assert_eq!(
        controls
            .find_command("permission")
            .expect("command fixture kept")
            .input_hint
            .as_deref(),
        Some("<preset>")
    );
}

#[test]
fn controls_catalog_fixture_parses_into_the_catalog() {
    let contract = contract();
    let result = &contract["fixtures"]["controlsCatalog"]["result"];
    let catalog = ModelCatalog::parse(result).expect("fixture parses");
    assert!(!catalog.is_empty());
    assert_eq!(catalog.groups[0].id, "deepseek-official");
    let reasoning = catalog
        .reasoning_for(&nano_controls::ModelSelection::new(
            "deepseek-official",
            "deepseek-flash",
        ))
        .expect("reasoning fixture kept");
    assert_eq!(reasoning.default_effort_name(), Some("High"));
    assert_eq!(catalog.failures[0].id, "third-party");
}
