use crate::error::{PluginError, Result};
use crate::native::NativePluginHost;
use crate::traits::{Plugin, PluginId, PluginInfo, PluginType};
use crate::wasm::{ResourceLimits, WasmPluginHost};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub search_paths: Vec<PathBuf>,

    #[serde(default)]
    pub plugins: HashMap<String, PluginEntry>,

    #[serde(default)]
    pub wasm: WasmConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    pub path: PathBuf,

    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub config: Value,
}

impl Default for PluginEntry {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            enabled: true,
            config: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WasmConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub max_memory_mb: Option<u32>,

    #[serde(default)]
    pub max_cpu_time_ms: Option<u32>,
}

pub struct PluginManager {
    native_host: NativePluginHost,
    wasm_host: WasmPluginHost,
    registry: PluginRegistry,
    config: PluginConfig,
}

struct PluginRegistry {
    origin_plugins: Vec<PluginId>,
    metadata_plugins: Vec<PluginId>,
    format_plugins: Vec<PluginId>,
}

impl PluginRegistry {
    fn new() -> Self {
        Self {
            origin_plugins: Vec::new(),
            metadata_plugins: Vec::new(),
            format_plugins: Vec::new(),
        }
    }

    fn register(&mut self, id: PluginId, plugin_type: PluginType) {
        match plugin_type {
            PluginType::Origin => {
                if !self.origin_plugins.contains(&id) {
                    self.origin_plugins.push(id);
                }
            }
            PluginType::Metadata => {
                if !self.metadata_plugins.contains(&id) {
                    self.metadata_plugins.push(id);
                }
            }
            PluginType::Format => {
                if !self.format_plugins.contains(&id) {
                    self.format_plugins.push(id);
                }
            }
        }
    }

    fn unregister(&mut self, id: PluginId) {
        self.origin_plugins.retain(|&x| x != id);
        self.metadata_plugins.retain(|&x| x != id);
        self.format_plugins.retain(|&x| x != id);
    }
}

impl PluginManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            native_host: NativePluginHost::new(),
            wasm_host: WasmPluginHost::new()?,
            registry: PluginRegistry::new(),
            config: PluginConfig::default(),
        })
    }

    pub fn init(config: &PluginConfig) -> Result<Self> {
        let mut manager = Self::new()?;
        manager.config = config.clone();

        if !config.enabled {
            info!("Plugin system disabled");
            return Ok(manager);
        }

        info!("Initializing plugin system");

        for path in &config.search_paths {
            manager.native_host.add_search_path(path.clone());
        }

        if config.wasm.enabled {
            let limits = ResourceLimits {
                max_memory_mb: config.wasm.max_memory_mb.unwrap_or(64),
                max_cpu_time_ms: config.wasm.max_cpu_time_ms.unwrap_or(5000),
                ..Default::default()
            };
            manager.wasm_host.set_limits(limits);
        }

        for (name, entry) in &config.plugins {
            if !entry.enabled {
                debug!("Skipping disabled plugin: {}", name);
                continue;
            }

            match manager.load_and_init(&entry.path, &entry.config) {
                Ok(id) => {
                    info!("Loaded plugin '{}' with id {:?}", name, id);
                }
                Err(e) => {
                    tracing::warn!("Failed to load plugin '{}': {}", name, e);
                }
            }
        }

        let discovered = manager.native_host.discover()?;
        for id in discovered {
            if let Some(info) = manager.native_host.list().iter().find(|i| i.id == id) {
                manager.registry.register(id, info.plugin_type);
            }
        }

        Ok(manager)
    }

    pub fn load_and_init(&mut self, path: &PathBuf, config: &Value) -> Result<PluginId> {
        let id = self.native_host.load(path)?;

        if let Some(plugin) = self.native_host.get_mut(id) {
            plugin.init(config.clone())?;
        }

        if let Some(info) = self.native_host.list().iter().find(|i| i.id == id) {
            self.registry.register(id, info.plugin_type);
        }

        Ok(id)
    }

    pub fn load_wasm(&mut self, wasm_bytes: &[u8]) -> Result<PluginId> {
        if !self.config.wasm.enabled {
            return Err(PluginError::Config("WASM plugins disabled".to_string()));
        }

        self.wasm_host.load(wasm_bytes)
    }

    pub fn unload(&mut self, id: PluginId) -> Result<()> {
        self.registry.unregister(id);

        if let Err(native_err) = self.native_host.unload(id) {
            if let Err(wasm_err) = self.wasm_host.unload(id) {
                return Err(PluginError::NotFound(format!(
                    "Plugin {:?} not found in native ({}) or WASM ({}) hosts",
                    id, native_err, wasm_err
                )));
            }
        }

        Ok(())
    }

    pub fn reload(&mut self, id: PluginId) -> Result<()> {
        self.native_host.reload(id)
    }

    pub fn reload_all(&mut self) -> Result<()> {
        let ids: Vec<PluginId> = self.native_host.list().iter().map(|i| i.id).collect();

        for id in ids {
            self.reload(id)?;
        }

        Ok(())
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let mut all = self.native_host.list();

        for (id, name) in self.wasm_host.list() {
            all.push(PluginInfo {
                id,
                name: name.to_string(),
                version: semver::Version::new(0, 0, 0),
                description: "WASM plugin".to_string(),
                plugin_type: PluginType::Origin,
            });
        }

        all
    }

    pub fn get(&self, id: PluginId) -> Option<&dyn Plugin> {
        self.native_host.get(id)
    }

    pub fn get_mut(&mut self, id: PluginId) -> Option<&mut dyn Plugin> {
        self.native_host.get_mut(id)
    }

    pub fn origin_plugin_ids(&self) -> &[PluginId] {
        &self.registry.origin_plugins
    }

    pub fn metadata_plugin_ids(&self) -> &[PluginId] {
        &self.registry.metadata_plugins
    }

    pub fn format_plugin_ids(&self) -> &[PluginId] {
        &self.registry.format_plugins
    }

    pub fn shutdown(&mut self) -> Result<()> {
        info!("Shutting down plugin system");

        let ids: Vec<PluginId> = self.list().iter().map(|i| i.id).collect();

        for id in ids {
            if let Err(e) = self.unload(id) {
                tracing::warn!("Failed to unload plugin {:?}: {}", id, e);
            }
        }

        Ok(())
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new().expect("Failed to create plugin manager")
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_manager_new() {
        let manager = PluginManager::new();
        assert!(manager.is_ok());
    }

    #[test]
    fn test_plugin_manager_disabled() {
        let config = PluginConfig {
            enabled: false,
            ..Default::default()
        };

        let manager = PluginManager::init(&config);
        assert!(manager.is_ok());

        let manager = manager.unwrap();
        assert!(manager.list().is_empty());
    }

    #[test]
    fn test_registry() {
        let mut registry = PluginRegistry::new();

        let id1 = PluginId::new(1);
        let id2 = PluginId::new(2);

        registry.register(id1, PluginType::Origin);
        registry.register(id2, PluginType::Metadata);

        assert_eq!(registry.origin_plugins.len(), 1);
        assert_eq!(registry.metadata_plugins.len(), 1);

        registry.unregister(id1);
        assert!(registry.origin_plugins.is_empty());
    }

    #[test]
    fn test_plugin_config_deserialize() {
        let json = r#"{
            "enabled": true,
            "search_paths": ["/usr/lib/musicfs/plugins"],
            "plugins": {
                "example": {
                    "path": "/path/to/plugin.so",
                    "enabled": true,
                    "config": {"key": "value"}
                }
            },
            "wasm": {
                "enabled": false
            }
        }"#;

        let config: PluginConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.search_paths.len(), 1);
        assert!(config.plugins.contains_key("example"));
    }
}
