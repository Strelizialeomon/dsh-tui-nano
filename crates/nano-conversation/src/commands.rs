//! Input routing. S02 decides who owns an input line; it never interprets
//! command business. `/` commands belong to S04, `/skill-name` to S05, and `!`
//! human commands to S08. `!!` has no special mode: it is ordinary shell text.

use crate::scene::{FeatureRegistry, SceneId};

/// The functional owner a routed input belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOwner {
    /// A registered command entry in the current scene.
    Registered(String),
    /// Model and permission commands (S04).
    Controls,
    /// Skills (S05).
    Skills,
    /// Manual shell commands (S08).
    Shell,
    /// A command that no feature registered yet.
    Unregistered(&'static str),
}

impl CommandOwner {
    /// Short diagnostics label.
    pub fn label(&self) -> &str {
        match self {
            Self::Registered(owner) => owner,
            Self::Controls => "S04",
            Self::Skills => "S05",
            Self::Shell => "S08",
            Self::Unregistered(owner) => owner,
        }
    }
}

/// What the input line means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputRoute {
    /// Ordinary user text; submit to the conversation.
    Prompt {
        /// Original text.
        text: String,
    },
    /// A registered command.
    Command {
        /// Registered owner.
        owner: String,
        /// Command name without the slash.
        name: String,
        /// Original arguments.
        args: String,
        /// Original line.
        raw: String,
    },
    /// A skill invocation.
    Skill {
        /// Skill name without the slash.
        name: String,
        /// Original arguments.
        args: String,
        /// Original line.
        raw: String,
    },
    /// A manual shell command.
    Bash {
        /// Command text after the leading `!`.
        command: String,
        /// Original line.
        raw: String,
    },
    /// A command or skill that this scene has no owner for yet.
    Unregistered {
        /// Whether the line looked like a command or a skill.
        owner: CommandOwner,
        /// Name without the slash.
        name: String,
        /// Original arguments.
        args: String,
        /// Original line.
        raw: String,
    },
}

impl InputRoute {
    /// Whether the route submits the line as an ordinary prompt.
    pub fn is_prompt(&self) -> bool {
        matches!(self, Self::Prompt { .. })
    }
}

/// Router over the static feature registry.
#[derive(Debug, Clone)]
pub struct CommandRouter<'a> {
    registry: &'a FeatureRegistry,
    scene: SceneId,
}

impl<'a> CommandRouter<'a> {
    /// Build a router for one scene.
    pub fn new(registry: &'a FeatureRegistry, scene: SceneId) -> Self {
        Self { registry, scene }
    }

    /// Route one input line. A line is never trimmed or rewritten.
    pub fn route(&self, text: &str) -> InputRoute {
        if let Some(rest) = text.strip_prefix('!') {
            return InputRoute::Bash {
                command: rest.to_string(),
                raw: text.to_string(),
            };
        }
        if text.starts_with('/') {
            return self.route_slash(text);
        }
        InputRoute::Prompt {
            text: text.to_string(),
        }
    }

    fn route_slash(&self, text: &str) -> InputRoute {
        let (token, args) = split_first_token(text);
        let name = token.trim_start_matches('/').to_string();
        let raw = text.to_string();
        if let Some(contribution) = self.registry.command_owner(&self.scene, token) {
            return InputRoute::Command {
                owner: contribution.id.0.clone(),
                name,
                args,
                raw,
            };
        }
        if self.registry.skills_owner(&self.scene).is_some() {
            return InputRoute::Skill { name, args, raw };
        }
        InputRoute::Unregistered {
            owner: CommandOwner::Unregistered("S04"),
            name,
            args,
            raw,
        }
    }
}

/// Split the first whitespace-delimited token from the rest. The remainder is
/// preserved exactly, including its leading whitespace.
fn split_first_token(text: &str) -> (&str, String) {
    match text.find(char::is_whitespace) {
        Some(index) => (&text[..index], text[index..].to_string()),
        None => (text, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{FeatureContribution, SceneId};

    #[test]
    fn plain_text_is_a_prompt() {
        let registry = FeatureRegistry::new();
        let router = CommandRouter::new(&registry, SceneId::conversation());
        assert_eq!(
            router.route("hello /world"),
            InputRoute::Prompt {
                text: "hello /world".into()
            }
        );
    }

    #[test]
    fn bang_routes_to_shell_without_rewriting() {
        let registry = FeatureRegistry::new();
        let router = CommandRouter::new(&registry, SceneId::conversation());
        assert_eq!(
            router.route("!ls -la"),
            InputRoute::Bash {
                command: "ls -la".into(),
                raw: "!ls -la".into()
            }
        );
        assert_eq!(
            router.route("!!echo hi"),
            InputRoute::Bash {
                command: "!echo hi".into(),
                raw: "!!echo hi".into()
            }
        );
    }

    #[test]
    fn registered_commands_keep_owner_and_arguments() {
        let mut registry = FeatureRegistry::new();
        registry.register(
            FeatureContribution::new("S04", SceneId::conversation()).with_command("/model"),
        );
        let router = CommandRouter::new(&registry, SceneId::conversation());
        assert_eq!(
            router.route("/model deepseek-chat"),
            InputRoute::Command {
                owner: "S04".into(),
                name: "model".into(),
                args: " deepseek-chat".into(),
                raw: "/model deepseek-chat".into()
            }
        );
    }

    #[test]
    fn skills_owner_routes_slash_names_to_s05() {
        let mut registry = FeatureRegistry::new();
        registry.register(FeatureContribution::new("S05", SceneId::conversation()).with_skills());
        let router = CommandRouter::new(&registry, SceneId::conversation());
        assert_eq!(
            router.route("/commit-msg --force"),
            InputRoute::Skill {
                name: "commit-msg".into(),
                args: " --force".into(),
                raw: "/commit-msg --force".into()
            }
        );
    }

    #[test]
    fn unregistered_slash_names_do_not_become_prompts() {
        let registry = FeatureRegistry::new();
        let router = CommandRouter::new(&registry, SceneId::conversation());
        match router.route("/anything") {
            InputRoute::Unregistered { owner, name, .. } => {
                assert_eq!(owner.label(), "S04");
                assert_eq!(name, "anything");
            }
            other => panic!("unexpected route {other:?}"),
        }
    }
}
