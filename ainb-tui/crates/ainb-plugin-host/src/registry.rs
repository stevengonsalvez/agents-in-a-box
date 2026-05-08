//! Empty registry shells.
//!
//! Phase 1 only needs the types to exist so downstream phases can refactor
//! ainb-core's enum-based dispatch into trait-object lookups. Each registry
//! holds a flat `Vec<RegisteredItem>`; richer indices (by id, by hotkey) land
//! when the registries actually get populated.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct ScreenRegistry {
    items: HashMap<String, ScreenSlot>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    items: HashMap<String, CommandSlot>,
}

#[derive(Debug, Clone, Default)]
pub struct SidebarRegistry {
    items: HashMap<String, SidebarSlot>,
}

#[derive(Debug, Clone, Default)]
pub struct StatuslineRegistry {
    items: HashMap<String, StatuslineSlot>,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    items: HashMap<String, ProviderSlot>,
}

#[derive(Debug, Clone)]
pub struct ScreenSlot {
    pub plugin_id: String,
    pub screen_id: String,
}

#[derive(Debug, Clone)]
pub struct CommandSlot {
    pub plugin_id: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct SidebarSlot {
    pub plugin_id: String,
    pub widget_id: String,
}

#[derive(Debug, Clone)]
pub struct StatuslineSlot {
    pub plugin_id: String,
    pub segment_id: String,
}

#[derive(Debug, Clone)]
pub struct ProviderSlot {
    pub plugin_id: String,
    pub provider_id: String,
}

macro_rules! impl_registry {
    ($reg:ident, $slot:ident, $field:ident) => {
        impl $reg {
            pub fn register(&mut self, plugin_id: &str, $field: impl Into<String>) {
                let key = $field.into();
                self.items.insert(
                    key.clone(),
                    $slot {
                        plugin_id: plugin_id.to_string(),
                        $field: key,
                    },
                );
            }

            #[must_use]
            pub fn len(&self) -> usize {
                self.items.len()
            }

            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.items.is_empty()
            }

            pub fn get(&self, key: &str) -> Option<&$slot> {
                self.items.get(key)
            }

            pub fn drop_plugin(&mut self, plugin_id: &str) {
                self.items.retain(|_, v| v.plugin_id != plugin_id);
            }
        }
    };
}

impl_registry!(ScreenRegistry, ScreenSlot, screen_id);
impl_registry!(CommandRegistry, CommandSlot, command);
impl_registry!(SidebarRegistry, SidebarSlot, widget_id);
impl_registry!(StatuslineRegistry, StatuslineSlot, segment_id);
impl_registry!(ProviderRegistry, ProviderSlot, provider_id);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_registry_register_and_drop() {
        let mut r = ScreenRegistry::default();
        assert!(r.is_empty());
        r.register("burndown", "analytics");
        r.register("kanban", "board");
        assert_eq!(r.len(), 2);
        r.drop_plugin("burndown");
        assert_eq!(r.len(), 1);
        assert!(r.get("board").is_some());
        assert!(r.get("analytics").is_none());
    }
}
