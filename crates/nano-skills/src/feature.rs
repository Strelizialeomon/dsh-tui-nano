//! Static feature contribution for the S02 registry.
//!
//! S05 claims slash-name routing and the two inspection panels; it does not
//! touch the global router.

use nano_conversation::{FeatureContribution, SceneId};

/// S05 contribution: slash names belong to skills, plus the skills and rules
/// panels. The final key table is frozen by S13's design contract.
pub fn contribution() -> FeatureContribution {
    FeatureContribution::new("S05", SceneId::conversation())
        .with_skills()
        .with_key("alt+k")
        .with_key("alt+r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contribution_owns_skill_routing_and_both_panels() {
        let contribution = contribution();
        assert_eq!(contribution.id.0, "S05");
        assert!(contribution.skills);
        assert!(contribution.keys.contains("alt+k"));
        assert!(contribution.keys.contains("alt+r"));
    }
}
