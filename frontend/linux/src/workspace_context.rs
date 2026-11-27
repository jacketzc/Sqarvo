//! Resource ownership shared by the tree, workspace and window commands.

use crate::proto::services::v1::{ObjectKind, ObjectRef};

/// Exact profile and service object identity. Runtime sessions are kept by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceIdentity {
    pub profile_id: String,
    pub object: ObjectRef,
}

impl ResourceIdentity {
    pub fn new(profile_id: &str, object: &ObjectRef) -> Self {
        let mut object = object.clone();
        object.session_id.clear();
        Self {
            profile_id: profile_id.to_owned(),
            object,
        }
    }
}

/// A command target can be a connection alone or have a database/schema scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTarget {
    pub profile_id: String,
    pub scope: Option<ObjectRef>,
    pub resource: Option<ResourceIdentity>,
}

impl ActionTarget {
    pub fn new(profile_id: &str, scope: Option<&ObjectRef>) -> Self {
        Self {
            profile_id: profile_id.to_owned(),
            scope: scope.map(|reference| ResourceIdentity::new(profile_id, reference).object),
            resource: None,
        }
    }

    pub fn with_resource(mut self, resource: ResourceIdentity) -> Self {
        debug_assert_eq!(self.profile_id, resource.profile_id);
        self.resource = Some(resource);
        self
    }

    pub fn scope_name(&self) -> Option<&str> {
        self.scope
            .as_ref()
            .map(|scope| scope.name.as_str())
            .filter(|name| !name.is_empty())
    }
}

/// The enclosing schema takes precedence over its catalog.
pub fn object_scope(reference: &ObjectRef) -> Option<ObjectRef> {
    let mut scope = reference.clone();
    match ObjectKind::try_from(reference.kind).ok() {
        Some(ObjectKind::Catalog | ObjectKind::Schema) => {}
        _ if !reference.schema.is_empty() => {
            scope.kind = ObjectKind::Schema as i32;
            scope.name.clone_from(&reference.schema);
        }
        _ if !reference.catalog.is_empty() => {
            scope.kind = ObjectKind::Catalog as i32;
            scope.name.clone_from(&reference.catalog);
        }
        _ => return None,
    }
    Some(scope)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TargetState {
    #[default]
    None,
    Available(ActionTarget),
    Unavailable(ActionTarget),
}

impl TargetState {
    pub fn resolve(navigation: Self, workspace: Self) -> Self {
        match navigation {
            Self::None => workspace,
            target => target,
        }
    }

    pub fn for_target(target: ActionTarget, available: bool) -> Self {
        if available {
            Self::Available(target)
        } else {
            Self::Unavailable(target)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_target_survives_unavailability_and_preserves_connection_only_scope() {
        let main = ObjectRef {
            kind: ObjectKind::Catalog as i32,
            name: "main".into(),
            ..Default::default()
        };
        let a = TargetState::Available(ActionTarget::new("a", Some(&main)));
        let b = TargetState::Available(ActionTarget::new("b", None));
        let offline_b = TargetState::Unavailable(ActionTarget::new("b", Some(&main)));
        assert_eq!(TargetState::resolve(b.clone(), a.clone()), b);
        assert_eq!(
            TargetState::resolve(offline_b.clone(), a.clone()),
            offline_b
        );
        assert_eq!(TargetState::resolve(TargetState::None, a.clone()), a);
    }
    #[test]
    fn object_identity_is_exact_and_independent_of_browse_session() {
        let original = ObjectRef {
            session_id: "browse-1".to_owned(),
            catalog: "main".to_owned(),
            schema: "shop".to_owned(),
            name: "Orders".to_owned(),
            kind: ObjectKind::Table as i32,
        };
        let expected = ResourceIdentity::new("profile-a", &original);
        assert_ne!(ResourceIdentity::new("profile-b", &original), expected);
        assert_eq!(
            ResourceIdentity::new(
                "profile-a",
                &ObjectRef {
                    session_id: "browse-2".to_owned(),
                    ..original.clone()
                }
            ),
            expected
        );
        for changed in [
            ObjectRef {
                catalog: "other".to_owned(),
                ..original.clone()
            },
            ObjectRef {
                schema: "other".to_owned(),
                ..original.clone()
            },
            ObjectRef {
                name: "orders".to_owned(),
                ..original.clone()
            },
            ObjectRef {
                kind: ObjectKind::View as i32,
                ..original.clone()
            },
        ] {
            assert_ne!(ResourceIdentity::new("profile-a", &changed), expected);
        }
        let scope = object_scope(&original).unwrap();
        assert_eq!(
            (scope.catalog.as_str(), scope.name.as_str(), scope.kind),
            ("main", "shop", ObjectKind::Schema as i32)
        );
    }
}
