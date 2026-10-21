//! Scene, focus, and static feature registration.
//!
//! S02 owns the single routing surface other features consume. Features are
//! explicit static contributions: a scene, command prefixes, keys, and a
//! visibility condition. There is no runtime plugin discovery and no second
//! global router.

use std::collections::BTreeSet;

use crate::address::SessionAddress;

/// Stable feature identity, such as `S03` or `conversation`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureId(pub String);

impl FeatureId {
    /// Build a feature id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for FeatureId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable scene identity. The main conversation is `conversation`; overlays
/// are focus entries on top of the active scene.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneId(pub String);

impl SceneId {
    /// Build a scene id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The main conversation scene.
    pub fn conversation() -> Self {
        Self("conversation".to_string())
    }
}

impl std::fmt::Display for SceneId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable focus identity. Only the top of the stack receives keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FocusId(pub String);

impl FocusId {
    /// Build a focus id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The input editor.
    pub fn input() -> Self {
        Self("input".to_string())
    }

    /// The queue dock.
    pub fn queue() -> Self {
        Self("queue".to_string())
    }
}

impl std::fmt::Display for FocusId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One static feature contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureContribution {
    /// Owner identity.
    pub id: FeatureId,
    /// Scenes this feature contributes to.
    pub scenes: BTreeSet<SceneId>,
    /// Command prefixes this feature owns, such as `/model`.
    pub command_prefixes: BTreeSet<String>,
    /// Whether the feature declares skill-name routing.
    pub skills: bool,
    /// Keys the feature claims while focused.
    pub keys: BTreeSet<String>,
    /// Whether the contribution is currently visible.
    pub visible: bool,
}

impl FeatureContribution {
    /// Build a visible contribution for one scene.
    pub fn new(id: impl Into<String>, scene: SceneId) -> Self {
        let mut scenes = BTreeSet::new();
        scenes.insert(scene);
        Self {
            id: FeatureId::new(id),
            scenes,
            command_prefixes: BTreeSet::new(),
            skills: false,
            keys: BTreeSet::new(),
            visible: true,
        }
    }

    /// Claim one command prefix.
    pub fn with_command(mut self, prefix: impl Into<String>) -> Self {
        self.command_prefixes.insert(prefix.into());
        self
    }

    /// Claim skill-name routing.
    pub fn with_skills(mut self) -> Self {
        self.skills = true;
        self
    }

    /// Claim one key binding while focused.
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.keys.insert(key.into());
        self
    }
}

/// Static registry of feature contributions.
#[derive(Debug, Clone, Default)]
pub struct FeatureRegistry {
    contributions: Vec<FeatureContribution>,
}

impl FeatureRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one contribution. A contribution becomes visible only when its
    /// owner explicitly registers it at startup.
    pub fn register(&mut self, contribution: FeatureContribution) {
        self.contributions.push(contribution);
    }

    /// All contributions.
    pub fn contributions(&self) -> &[FeatureContribution] {
        &self.contributions
    }

    /// Visible contributions for one scene.
    pub fn visible_for<'a>(
        &'a self,
        scene: &SceneId,
    ) -> impl Iterator<Item = &'a FeatureContribution> + 'a {
        let scene = scene.clone();
        self.contributions.iter().filter(move |contribution| {
            contribution.visible && contribution.scenes.contains(&scene)
        })
    }

    /// Owner of one command prefix in the active scene, if any.
    pub fn command_owner(&self, scene: &SceneId, prefix: &str) -> Option<&FeatureContribution> {
        self.visible_for(scene)
            .find(|contribution| contribution.command_prefixes.contains(prefix))
    }

    /// Whether any visible feature in the active scene claims skill-name routing.
    pub fn skills_owner(&self, scene: &SceneId) -> Option<&FeatureContribution> {
        self.visible_for(scene)
            .find(|contribution| contribution.skills)
    }

    /// Whether any visible feature claims this key in the scene.
    pub fn key_owner(&self, scene: &SceneId, key: &str) -> Option<&FeatureContribution> {
        self.visible_for(scene)
            .find(|contribution| contribution.keys.contains(key))
    }
}

/// Focus stack for the active scene. The top receives input; losing focus
/// never moves a draft into another address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusStack {
    stack: Vec<FocusId>,
}

impl Default for FocusStack {
    fn default() -> Self {
        Self {
            stack: vec![FocusId::input()],
        }
    }
}

impl FocusStack {
    /// Build a stack with the given bottom focus.
    pub fn new(base: FocusId) -> Self {
        Self { stack: vec![base] }
    }

    /// The focus receiving keys.
    pub fn top(&self) -> Option<&FocusId> {
        self.stack.last()
    }

    /// Push an overlay focus.
    pub fn push(&mut self, focus: FocusId) {
        if self.stack.last() != Some(&focus) {
            self.stack.push(focus);
        }
    }

    /// Pop the top focus; the base focus is never removed.
    pub fn pop(&mut self) -> Option<FocusId> {
        if self.stack.len() <= 1 {
            return None;
        }
        self.stack.pop()
    }

    /// Replace the stack with a single focus.
    pub fn reset(&mut self, focus: FocusId) {
        self.stack.clear();
        self.stack.push(focus);
    }

    /// Current depth.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Whether the input editor currently receives keys.
    pub fn input_focused(&self) -> bool {
        self.top() == Some(&FocusId::input())
    }
}

/// One main scene plus its focus stack, address, and reading anchor slot.
#[derive(Debug, Clone)]
pub struct SceneState {
    active: SceneId,
    focus: FocusStack,
    address: Option<SessionAddress>,
    reading_anchor: Option<String>,
}

impl SceneState {
    /// Build the conversation scene with input focused.
    pub fn conversation() -> Self {
        Self {
            active: SceneId::conversation(),
            focus: FocusStack::default(),
            address: None,
            reading_anchor: None,
        }
    }

    /// Address currently displayed by the scene. Switching scenes keeps it;
    /// a caller that changes conversations updates it explicitly.
    pub fn address(&self) -> Option<&SessionAddress> {
        self.address.as_ref()
    }

    /// Bind the scene to one conversation address.
    pub fn set_address(&mut self, address: SessionAddress) {
        self.address = Some(address);
    }

    /// Reading anchor slot consumed by S03; S02 preserves it across switches.
    pub fn reading_anchor(&self) -> Option<&str> {
        self.reading_anchor.as_deref()
    }

    /// Store a reading anchor without interpreting it.
    pub fn set_reading_anchor(&mut self, anchor: Option<String>) {
        self.reading_anchor = anchor;
    }

    /// Active scene.
    pub fn active(&self) -> &SceneId {
        &self.active
    }

    /// Focus stack.
    pub fn focus(&self) -> &FocusStack {
        &self.focus
    }

    /// Focus stack, mutably.
    pub fn focus_mut(&mut self) -> &mut FocusStack {
        &mut self.focus
    }

    /// Switch the main scene and reset focus to input. Overlays are popped;
    /// the address-bound drafts are owned by the draft store, not the scene.
    /// The address and reading anchor survive a scene switch.
    pub fn switch(&mut self, scene: SceneId) {
        self.active = scene;
        self.focus.reset(FocusId::input());
    }

    /// Switch to a scene bound to a different address, dropping the old
    /// reading anchor because it belonged to the old transcript.
    pub fn switch_to(&mut self, scene: SceneId, address: SessionAddress) {
        self.address = Some(address);
        self.reading_anchor = None;
        self.switch(scene);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_stack_routes_only_to_the_top() {
        let mut stack = FocusStack::default();
        assert!(stack.input_focused());
        stack.push(FocusId::queue());
        assert!(!stack.input_focused());
        assert_eq!(stack.top().unwrap().0, "queue");
        assert_eq!(stack.pop().unwrap().0, "queue");
        assert!(stack.input_focused());
        assert!(stack.pop().is_none());
    }

    #[test]
    fn scene_switch_keeps_address_and_anchor_but_change_of_address_clears_the_anchor() {
        let mut scene = SceneState::conversation();
        scene.set_address(crate::address::SessionAddress::root("root"));
        scene.set_reading_anchor(Some("msg-7".to_string()));
        scene.focus_mut().push(FocusId::queue());
        scene.switch(SceneId::new("transcript"));
        assert!(scene.focus().input_focused());
        assert_eq!(scene.address().unwrap().root_session_id, "root");
        assert_eq!(scene.reading_anchor(), Some("msg-7"));
        scene.switch_to(
            SceneId::conversation(),
            crate::address::SessionAddress::root("other"),
        );
        assert_eq!(scene.address().unwrap().root_session_id, "other");
        assert!(scene.reading_anchor().is_none());
    }

    #[test]
    fn registry_is_static_and_scene_scoped() {
        let mut registry = FeatureRegistry::new();
        registry.register(
            FeatureContribution::new("conversation", SceneId::conversation())
                .with_command("/")
                .with_key("enter"),
        );
        registry.register(
            FeatureContribution::new("S04", SceneId::conversation()).with_command("/model"),
        );
        assert_eq!(
            registry
                .command_owner(&SceneId::conversation(), "/model")
                .unwrap()
                .id
                .0,
            "S04"
        );
        assert_eq!(
            registry
                .command_owner(&SceneId::conversation(), "/")
                .unwrap()
                .id
                .0,
            "conversation"
        );
        assert!(registry
            .command_owner(&SceneId::new("controls"), "/")
            .is_none());
        assert_eq!(
            registry
                .key_owner(&SceneId::conversation(), "enter")
                .unwrap()
                .id
                .0,
            "conversation"
        );
    }
}
