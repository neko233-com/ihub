//! Pure planning and renderer-safe projection for manifest-owned plugin
//! global shortcuts.
//!
//! Operating-system registration remains in `app.rs`; this module makes every
//! conflict and lifecycle decision deterministic and unit-testable before a
//! native shortcut API is touched.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::models::PluginInfo;

pub(crate) const MAX_REGISTERED_PLUGIN_SHORTCUTS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PluginShortcutTarget {
    Command(String),
    Keyword(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginShortcutBinding {
    pub key: String,
    pub plugin_id: String,
    pub shortcut: String,
    pub target: PluginShortcutTarget,
    pub auto_copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginShortcutStatus {
    pub registration: String,
    pub error: Option<String>,
}

impl PluginShortcutStatus {
    pub fn registered() -> Self {
        Self {
            registration: "registered".to_owned(),
            error: None,
        }
    }

    pub fn unavailable(error: impl Into<String>) -> Self {
        Self {
            registration: "unavailable".to_owned(),
            error: Some(error.into()),
        }
    }

    fn blocked(error: impl Into<String>) -> Self {
        Self {
            registration: "blocked".to_owned(),
            error: Some(error.into()),
        }
    }

    fn inactive() -> Self {
        Self {
            registration: "inactive".to_owned(),
            error: Some("插件已停用；其全局快捷键没有注册。".to_owned()),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct PluginShortcutRegistry {
    pub active: HashMap<String, PluginShortcutBinding>,
    pub statuses: HashMap<String, PluginShortcutStatus>,
}

#[derive(Debug, Default)]
pub(crate) struct PluginShortcutPlan {
    pub ready: Vec<PluginShortcutBinding>,
    pub statuses: HashMap<String, PluginShortcutStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginShortcutEvent {
    pub plugin_id: String,
    pub shortcut: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

impl PluginShortcutEvent {
    pub fn from_binding(binding: &PluginShortcutBinding) -> Self {
        let (command_id, keyword) = match &binding.target {
            PluginShortcutTarget::Command(command_id) => (Some(command_id.clone()), None),
            PluginShortcutTarget::Keyword(keyword) => (None, Some(keyword.clone())),
        };
        Self {
            plugin_id: binding.plugin_id.clone(),
            shortcut: binding.shortcut.clone(),
            command_id,
            keyword,
            input: None,
        }
    }
}

pub(crate) fn command_binding_key(plugin_id: &str, command_id: &str) -> String {
    format!("command:{plugin_id}:{command_id}")
}

pub(crate) fn global_binding_key(plugin_id: &str, binding_id: &str) -> String {
    format!("global:{plugin_id}:{binding_id}")
}

fn shortcut_candidates(plugins: &[PluginInfo]) -> Vec<(bool, PluginShortcutBinding)> {
    let mut candidates = Vec::new();
    for plugin in plugins {
        for command in &plugin.commands {
            let Some(shortcut) = command.shortcut.as_ref() else {
                continue;
            };
            candidates.push((
                plugin.enabled,
                PluginShortcutBinding {
                    key: command_binding_key(&plugin.id, &command.id),
                    plugin_id: plugin.id.clone(),
                    shortcut: shortcut.clone(),
                    target: PluginShortcutTarget::Command(command.id.clone()),
                    auto_copy: false,
                },
            ));
        }
        for shortcut in &plugin.global_shortcuts {
            let target = match (&shortcut.command_id, &shortcut.keyword) {
                (Some(command_id), None) => PluginShortcutTarget::Command(command_id.clone()),
                (None, Some(keyword)) => PluginShortcutTarget::Keyword(keyword.clone()),
                // Rust manifest validation rejects this. Ignore malformed
                // legacy projections instead of inventing an activation.
                _ => continue,
            };
            candidates.push((
                plugin.enabled,
                PluginShortcutBinding {
                    key: global_binding_key(&plugin.id, &shortcut.id),
                    plugin_id: plugin.id.clone(),
                    shortcut: shortcut.shortcut.clone(),
                    target,
                    auto_copy: false,
                },
            ));
        }
    }
    candidates
}

pub(crate) fn plan_plugin_shortcuts(
    plugins: &[PluginInfo],
    launcher_reserved: &HashSet<String>,
) -> PluginShortcutPlan {
    let candidates = shortcut_candidates(plugins);
    let mut shortcut_owners = HashMap::<String, usize>::new();
    for (enabled, binding) in &candidates {
        if *enabled {
            *shortcut_owners.entry(binding.shortcut.clone()).or_default() += 1;
        }
    }

    let mut plan = PluginShortcutPlan::default();
    for (enabled, binding) in candidates {
        let status = if !enabled {
            Some(PluginShortcutStatus::inactive())
        } else if launcher_reserved.contains(&binding.shortcut) {
            Some(PluginShortcutStatus::blocked(format!(
                "快捷键 {} 与 iHub 启动器或恢复快捷键冲突。",
                binding.shortcut
            )))
        } else if shortcut_owners
            .get(binding.shortcut.as_str())
            .copied()
            .unwrap_or_default()
            > 1
        {
            Some(PluginShortcutStatus::blocked(format!(
                "快捷键 {} 被多个插件声明；为避免顺序劫持，所有重复声明均未注册。",
                binding.shortcut
            )))
        } else if plan.ready.len() >= MAX_REGISTERED_PLUGIN_SHORTCUTS {
            Some(PluginShortcutStatus::blocked(format!(
                "插件快捷键总数超过宿主上限 {MAX_REGISTERED_PLUGIN_SHORTCUTS}。"
            )))
        } else {
            None
        };
        if let Some(status) = status {
            plan.statuses.insert(binding.key.clone(), status);
        } else {
            plan.ready.push(binding);
        }
    }
    plan
}

pub(crate) fn apply_plugin_shortcut_statuses(
    plugins: &mut [PluginInfo],
    statuses: &HashMap<String, PluginShortcutStatus>,
) {
    for plugin in plugins {
        for command in &mut plugin.commands {
            if command.shortcut.is_none() {
                continue;
            }
            let status = statuses.get(&command_binding_key(&plugin.id, &command.id));
            command.shortcut_registration = Some(
                status
                    .map(|status| status.registration.clone())
                    .unwrap_or_else(|| "unavailable".to_owned()),
            );
            command.shortcut_error = status
                .and_then(|status| status.error.clone())
                .or_else(|| Some("插件快捷键注册状态暂不可用。".to_owned()));
            if status.is_some_and(|status| status.error.is_none()) {
                command.shortcut_error = None;
            }
        }
        for shortcut in &mut plugin.global_shortcuts {
            let status = statuses.get(&global_binding_key(&plugin.id, &shortcut.id));
            shortcut.registration = status
                .map(|status| status.registration.clone())
                .unwrap_or_else(|| "unavailable".to_owned());
            shortcut.error = status
                .and_then(|status| status.error.clone())
                .or_else(|| Some("插件快捷键注册状态暂不可用。".to_owned()));
            if status.is_some_and(|status| status.error.is_none()) {
                shortcut.error = None;
            }
        }
    }
}

pub(crate) fn binding_is_current(plugins: &[PluginInfo], binding: &PluginShortcutBinding) -> bool {
    shortcut_candidates(plugins)
        .into_iter()
        .any(|(enabled, candidate)| {
            enabled
                && candidate.key == binding.key
                && candidate.plugin_id == binding.plugin_id
                && candidate.shortcut == binding.shortcut
                && candidate.target == binding.target
        })
}

/// A detached host owns only visible frontend commands for its exact plugin.
///
/// Keyword shortcuts still belong to the main launcher search field, while
/// native commands must retain the main host's explicit worker approval path.
/// The legacy frontend fallback mirrors the renderer activation rule for
/// packages that predate per-command execution metadata.
pub(crate) fn binding_targets_frontend_command(
    plugins: &[PluginInfo],
    binding: &PluginShortcutBinding,
) -> bool {
    let PluginShortcutTarget::Command(command_id) = &binding.target else {
        return false;
    };
    let Some(plugin) = plugins
        .iter()
        .find(|plugin| plugin.enabled && plugin.id == binding.plugin_id)
    else {
        return false;
    };
    let Some(command) = plugin
        .commands
        .iter()
        .find(|command| command.id == *command_id)
    else {
        return false;
    };
    command.execution == "frontend"
        || (plugin.frontend_entry.is_some() && !plugin.has_native_worker)
}

#[cfg(test)]
mod tests {
    use crate::models::{PluginCommandInfo, PluginGlobalShortcutInfo};

    use super::*;

    fn plugin(id: &str, shortcut: &str) -> PluginInfo {
        PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            version: "1.0.0".to_owned(),
            description: None,
            compatibility: "ihub".to_owned(),
            icon_src: None,
            source: None,
            commit: None,
            installed_at: None,
            source_lock: None,
            is_development_link: false,
            local_link_status: None,
            local_link_error: None,
            uses_managed_snapshot_fallback: false,
            local_path: None,
            frontend_entry: Some("dist/index.html".to_owned()),
            enabled: true,
            has_native_worker: false,
            update_channel: None,
            auto_update: false,
            command_count: 1,
            tool_count: 0,
            commands: vec![PluginCommandInfo {
                id: "open".to_owned(),
                name: "Open".to_owned(),
                description: None,
                icon_src: None,
                execution: "frontend".to_owned(),
                keywords: vec!["open".to_owned()],
                utools_text_matchers: Vec::new(),
                shortcut: Some(shortcut.to_owned()),
                shortcut_registration: None,
                shortcut_error: None,
            }],
            global_shortcuts: Vec::new(),
            search_providers: Vec::new(),
            launcher_context: None,
        }
    }

    #[test]
    fn duplicate_and_launcher_conflicts_fail_closed_for_every_owner() {
        let plugins = vec![
            plugin("plugin-a", "Alt+KeyA"),
            plugin("plugin-b", "Alt+KeyA"),
            plugin("plugin-c", "CmdOrCtrl+KeyC"),
        ];
        let reserved = HashSet::from(["CmdOrCtrl+KeyC".to_owned()]);
        let plan = plan_plugin_shortcuts(&plugins, &reserved);

        assert!(plan.ready.is_empty());
        assert_eq!(plan.statuses.len(), 3);
        assert!(plan
            .statuses
            .values()
            .all(|status| status.registration == "blocked"));
    }

    #[test]
    fn disabled_mappings_are_visible_but_never_planned_for_registration() {
        let mut disabled = plugin("plugin-a", "Alt+KeyA");
        disabled.enabled = false;
        disabled.global_shortcuts.push(PluginGlobalShortcutInfo {
            id: "find".to_owned(),
            shortcut: "Alt+KeyF".to_owned(),
            command_id: None,
            keyword: Some("find".to_owned()),
            registration: "inactive".to_owned(),
            error: None,
        });
        let plan = plan_plugin_shortcuts(&[disabled], &HashSet::new());
        assert!(plan.ready.is_empty());
        assert_eq!(plan.statuses.len(), 2);
        assert!(plan
            .statuses
            .values()
            .all(|status| status.registration == "inactive"));
    }

    #[test]
    fn status_projection_never_hides_a_missing_native_result() {
        let mut plugins = vec![plugin("plugin-a", "Alt+KeyA")];
        apply_plugin_shortcut_statuses(&mut plugins, &HashMap::new());
        assert_eq!(
            plugins[0].commands[0].shortcut_registration.as_deref(),
            Some("unavailable")
        );
        assert!(plugins[0].commands[0].shortcut_error.is_some());
    }

    #[test]
    fn current_binding_check_includes_target_and_lifecycle() {
        let mut plugins = vec![plugin("plugin-a", "Alt+KeyA")];
        let binding = plan_plugin_shortcuts(&plugins, &HashSet::new())
            .ready
            .pop()
            .unwrap();
        assert!(binding_is_current(&plugins, &binding));
        plugins[0].enabled = false;
        assert!(!binding_is_current(&plugins, &binding));
    }

    #[test]
    fn detached_routing_accepts_only_the_exact_frontend_command() {
        let mut plugins = vec![plugin("plugin-a", "Alt+KeyA")];
        let binding = plan_plugin_shortcuts(&plugins, &HashSet::new())
            .ready
            .pop()
            .unwrap();
        assert!(binding_targets_frontend_command(&plugins, &binding));

        plugins[0].commands[0].execution = "native".to_owned();
        plugins[0].has_native_worker = true;
        assert!(!binding_targets_frontend_command(&plugins, &binding));

        plugins[0].commands[0].execution = "frontend".to_owned();
        plugins[0].enabled = false;
        assert!(!binding_targets_frontend_command(&plugins, &binding));
    }

    #[test]
    fn detached_routing_keeps_keyword_shortcuts_in_the_main_launcher() {
        let plugins = vec![plugin("plugin-a", "Alt+KeyA")];
        let binding = PluginShortcutBinding {
            key: global_binding_key("plugin-a", "find"),
            plugin_id: "plugin-a".to_owned(),
            shortcut: "Alt+KeyF".to_owned(),
            target: PluginShortcutTarget::Keyword("find".to_owned()),
            auto_copy: false,
        };
        assert!(!binding_targets_frontend_command(&plugins, &binding));
    }
}
