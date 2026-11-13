//! Provider availability is scoped to complete resource identities. Base ABI
//! configuration remains a separate projection for native documents.
use super::*;
use loog_extension_host::{ProviderConfiguration as Configuration, ProviderMode};
use loog_workspace_api::session::{LanguageMode, Reply, ResourceProviderConfiguration as Wire};
use loog_workspace_api::{Failure, FailureKind, Owner, ResourceRef};
use std::collections::{BTreeMap, HashSet};

struct Group {
    wire: Wire,
    configuration: Configuration,
}
struct Set {
    groups: Vec<Group>,
    disabled: Configuration,
}
#[derive(Default)]
pub(super) struct Configurations {
    owners: BTreeMap<Owner, Set>,
}
impl Configurations {
    pub(super) fn retire(&mut self, id: &str) {
        self.owners.retain(|owner, _| owner.id != id);
    }
    pub(super) fn get(
        &self,
        owner: &Owner,
        provider: &str,
        resource: &ResourceRef,
    ) -> Option<&Configuration> {
        self.owners.get(owner).map(|set| {
            set.groups
                .iter()
                .find(|group| {
                    group.wire.provider == provider && group.wire.contexts.contains(resource)
                })
                .map_or(&set.disabled, |group| &group.configuration)
        })
    }
}
fn configuration(wire: &Wire) -> Configuration {
    Configuration {
        provider: wire.provider.clone(),
        enabled: wire.enabled,
        revision: Some(wire.revision),
        modes: wire.modes.as_ref().map(|modes| {
            modes
                .iter()
                .map(|mode| match mode {
                    LanguageMode::Definition => ProviderMode::Definition,
                    LanguageMode::Declaration => ProviderMode::Declaration,
                    LanguageMode::TypeDefinition => ProviderMode::TypeDefinition,
                    LanguageMode::Implementation => ProviderMode::Implementation,
                    LanguageMode::Full => ProviderMode::Full,
                    LanguageMode::Range => ProviderMode::Range,
                })
                .collect()
        }),
        resolve: wire.resolve,
        prepare: wire.prepare,
        triggers: wire.triggers.clone(),
        retriggers: wire.retriggers.clone(),
        commands: wire.commands.clone(),
        aliases: wire.aliases.clone(),
    }
}
impl ExtensionService {
    pub(super) fn provider_configuration<'a>(
        &'a self,
        package: &'a Package,
        provider: &str,
        resource: Option<&ResourceRef>,
    ) -> Option<&'a Configuration> {
        match resource.filter(|r| r.environment().is_some()) {
            Some(resource) => self.resource_configurations.get(
                &Owner {
                    id: package.manifest.id.clone(),
                    generation: package.lifecycle.generation(),
                },
                provider,
                resource,
            ),
            // COMPAT(native-provider-configuration): Base ABI options are still
            // interpreted for native paths. Resource configurations replace this
            // projection only for provided documents; no removal is scheduled.
            None => package.provider_configurations.get(provider),
        }
    }
    pub(super) fn configure_resource_providers(
        &mut self,
        owner: &Owner,
        configurations: Vec<Wire>,
    ) -> Result<(Reply, Vec<Effect>), Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if ["documents.read", "languages"]
            .iter()
            .any(|c| self.authorize(package, c).is_err())
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Language configuration is not granted",
            ));
        }
        if configurations.len() > 128
            || serde_json::to_vec(&configurations).map_or(true, |bytes| bytes.len() > 512 * 1024)
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Language configuration budget exceeded",
            ));
        }
        let mut seen = HashSet::new();
        for item in &configurations {
            if !self.resource_documents.bound(owner, &item.provider)
                || item.contexts.is_empty()
                || item.contexts.len() > 128
                || item.modes.as_ref().is_some_and(|m| m.len() > 6)
                || [&item.triggers, &item.retriggers].iter().any(|values| {
                    values.len() > 64
                        || values
                            .iter()
                            .any(|v| v.is_empty() || v.len() > 16 || v.contains('\0'))
                })
                || [&item.commands, &item.aliases].iter().any(|values| {
                    values.len() > 64
                        || values.iter().any(|v| {
                            v.is_empty() || v.len() > 256 || v.contains(['\0', '\r', '\n'])
                        })
                })
                || item.contexts.iter().any(|resource| {
                    resource.environment().is_none()
                        || !seen.insert((item.provider.clone(), resource.clone()))
                })
            {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid or overlapping resource provider configuration",
                ));
            }
        }
        if self
            .resource_configurations
            .owners
            .get(owner)
            .is_some_and(|set| set.groups.iter().map(|g| &g.wire).eq(configurations.iter()))
        {
            return Ok((Reply::Done, vec![]));
        }
        let before = self.resource_configurations.owners.remove(owner);
        let disabled = Configuration {
            provider: String::new(),
            enabled: false,
            modes: Some(vec![]),
            revision: None,
            resolve: false,
            prepare: false,
            triggers: vec![],
            retriggers: vec![],
            commands: vec![],
            aliases: vec![],
        };
        self.resource_configurations.owners.insert(
            owner.clone(),
            Set {
                groups: configurations
                    .into_iter()
                    .map(|wire| Group {
                        configuration: configuration(&wire),
                        wire,
                    })
                    .collect(),
                disabled,
            },
        );
        self.packages.get_mut(&owner.id).unwrap().provider_revision += 1;
        let pending = self
            .provider_calls
            .iter()
            .filter(|(_, call)| {
                if call.extension != owner.id
                    || call.generation != owner.generation
                    || call.resource.environment().is_none()
                {
                    return false;
                }
                let next = self
                    .resource_configurations
                    .get(owner, &call.provider, &call.resource)
                    .unwrap();
                let previous = before
                    .as_ref()
                    .and_then(|set| {
                        set.groups.iter().find(|g| {
                            g.wire.provider == call.provider
                                && g.wire.contexts.contains(&call.resource)
                        })
                    })
                    .map(|g| &g.configuration);
                !next.enabled
                    || previous.is_some_and(|old| !language::same_configuration(old, next))
            })
            .map(|(request, _)| *request)
            .collect::<Vec<_>>();
        let mut effects = vec![];
        for request in pending {
            self.cancel_provider(request, &mut effects);
        }
        if let Some(before) = before {
            for group in before.groups {
                for resource in group.wire.contexts {
                    if !self
                        .resource_configurations
                        .get(owner, &group.wire.provider, &resource)
                        .unwrap()
                        .enabled
                    {
                        if let Some(source) = self
                            .sources
                            .get(&(owner.id.clone(), group.wire.provider.clone()))
                        {
                            effects.push(Effect::ClearResourceDiagnostics {
                                source: *source,
                                resource,
                            });
                        }
                    }
                }
            }
        }
        Ok((Reply::Done, effects))
    }
}
