//! Effective permissions: the grant a role acts under for an action on an entity.
//!
//! Exactly one role is evaluated per request (`Caller::role()`: `anonymous` or `authenticated`).
//! For a role and action:
//!
//! 1. the role's own entries covering the action; when several do (`read` and `*`), the last one
//!    wins, as DAB's `AuthorizationResolver` overwrites the operation's metadata entry by entry;
//! 2. otherwise, for `authenticated`, the `anonymous` grant only when the entity has no
//!    explicit `authenticated` permission entry;
//! 3. otherwise the action is denied.

use rl_model::{Action, ActionPermission, Entity};

pub const ANONYMOUS: &str = "anonymous";
pub const AUTHENTICATED: &str = "authenticated";
const ROLES: [&str; 2] = [ANONYMOUS, AUTHENTICATED];

/// The grant `role` acts under for `action` on `entity`, `None` when denied.
pub fn grant<'m>(entity: &'m Entity, role: &str, action: Action) -> Option<&'m ActionPermission> {
    entity.grants(role, action).last().or_else(|| {
        if role == AUTHENTICATED && entity.role(AUTHENTICATED).is_none() {
            entity.grants(ANONYMOUS, action).last()
        } else {
            None
        }
    })
}

/// Whether the grant lets the role use the exposed field `field`.
pub fn allows_field(grant: &ActionPermission, field: &str) -> bool {
    grant
        .fields
        .as_ref()
        .is_none_or(|access| access.allows(field))
}

/// Whether some role may read the entity (DAB then exposes its query fields).
pub fn readable_by_anyone(entity: &Entity) -> bool {
    ROLES
        .iter()
        .any(|role| grant(entity, role, Action::Read).is_some())
}

/// Whether some role may read `field` (DAB then exposes it on the entity's type).
pub fn field_readable_by_anyone(entity: &Entity, field: &str) -> bool {
    ROLES.iter().any(|role| {
        grant(entity, role, Action::Read).is_some_and(|grant| allows_field(grant, field))
    })
}

#[cfg(test)]
mod tests {
    use rl_model::{AppModel, FieldAccess, RolePermission};

    use super::*;

    fn basic() -> AppModel {
        let json = include_str!("../../rl-model/tests/fixtures/basic.analysis.json");
        AppModel::from_analysis(&serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn authenticated_falls_back_only_without_an_authenticated_entry() {
        let model = basic();
        let project = model.entity("Project").unwrap();
        let anonymous = grant(project, ANONYMOUS, Action::Read).unwrap();
        let authenticated = grant(project, AUTHENTICATED, Action::Read).unwrap();
        assert_ne!(anonymous.policy, authenticated.policy);

        let milestone = model.entity("Milestone").unwrap();
        assert!(grant(milestone, ANONYMOUS, Action::Read).is_none());
        assert!(grant(milestone, AUTHENTICATED, Action::Delete).is_some());

        let todo = model.entity("Todo").unwrap();
        let anonymous = grant(todo, ANONYMOUS, Action::Read).unwrap();
        assert!(allows_field(anonymous, "title") && !allows_field(anonymous, "cost"));
        assert!(field_readable_by_anyone(todo, "cost"));
        assert!(!field_readable_by_anyone(todo, "secretNote"));
        // Comment: anonymous reads without authorEmail; authenticated has its own read.
        let comment = model.entity("Comment").unwrap();
        let read = grant(comment, AUTHENTICATED, Action::Read).unwrap();
        assert!(allows_field(read, "authorEmail") && read.policy.is_none());
    }

    #[test]
    fn authenticated_entry_blocks_inheritance_even_for_unlisted_actions() {
        let model = basic();
        let mut entity = model.entity("Category").unwrap().clone();
        let read = ActionPermission {
            action: Action::Read,
            fields: None,
            policy: None,
        };
        entity.permissions = vec![RolePermission {
            role: ANONYMOUS.into(),
            actions: vec![read.clone()],
        }];
        assert_eq!(grant(&entity, AUTHENTICATED, Action::Read), Some(&read));
        entity.permissions.push(RolePermission {
            role: AUTHENTICATED.into(),
            actions: vec![ActionPermission {
                action: Action::Update,
                ..read.clone()
            }],
        });
        assert!(grant(&entity, AUTHENTICATED, Action::Read).is_none());
        assert!(grant(&entity, AUTHENTICATED, Action::Update).is_some());
        assert_eq!(grant(&entity, ANONYMOUS, Action::Read), Some(&read));
        entity.permissions.last_mut().unwrap().actions.clear();
        assert!(grant(&entity, AUTHENTICATED, Action::Read).is_none());
    }

    #[test]
    fn last_matching_action_replaces_policy_and_fields_including_wildcards() {
        let model = basic();
        let mut entity = model.entity("Todo").unwrap().clone();
        let restricted = grant(&entity, AUTHENTICATED, Action::Read).unwrap().clone();
        let wildcard = ActionPermission {
            action: Action::All,
            fields: None,
            policy: None,
        };
        for actions in [
            vec![restricted.clone(), wildcard.clone()],
            vec![wildcard.clone(), restricted.clone()],
            vec![
                restricted.clone(),
                ActionPermission {
                    action: Action::Read,
                    fields: Some(FieldAccess {
                        include: vec!["id".into(), "title".into()],
                        exclude: vec!["title".into()],
                    }),
                    policy: None,
                },
            ],
        ] {
            let expected = actions.last().unwrap().clone();
            entity.permissions = vec![RolePermission {
                role: AUTHENTICATED.into(),
                actions,
            }];
            let effective = grant(&entity, AUTHENTICATED, Action::Read).unwrap();
            assert_eq!(effective, &expected);
            assert_eq!(effective.policy, expected.policy);
            if effective
                .fields
                .as_ref()
                .is_some_and(|fields| fields.exclude.iter().any(|field| field == "title"))
            {
                assert!(allows_field(effective, "id"));
                assert!(!allows_field(effective, "title"));
                assert!(!allows_field(effective, "cost"));
            }
        }
    }
}
