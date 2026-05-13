use crate::error::{PluginError, Result};
use crate::traits::{Plugin, PluginId, PluginInfo, PluginType, PLUGIN_API_VERSION};
use libloading::{Library, Symbol};
use semver::Version;
use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

static NEXT_PLUGIN_ID: AtomicU64 = AtomicU64::new(1);

fn next_plugin_id() -> PluginId {
    PluginId::new(NEXT_PLUGIN_ID.fetch_add(1, Ordering::SeqCst))
}

struct LoadedPlugin {
    id: PluginId,
    path: PathBuf,
    library: Library,
    instance: Box<dyn Plugin>,
    plugin_type: PluginType,
}

pub struct NativePluginHost {
    plugins: HashMap<PluginId, LoadedPlugin>,
    search_paths: Vec<PathBuf>,
}

impl NativePluginHost {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            search_paths: Vec::new(),
        }
    }

    pub fn add_search_path(&mut self, path: PathBuf) {
        if !self.search_paths.contains(&path) {
            self.search_paths.push(path);
        }
    }

    pub fn load(&mut self, path: &Path) -> Result<PluginId> {
        let canonical = path.canonicalize().map_err(|e| {
            PluginError::LoadFailed(format!("Cannot resolve path {}: {}", path.display(), e))
        })?;

        for plugin in self.plugins.values() {
            if plugin.path == canonical {
                return Err(PluginError::AlreadyLoaded(canonical.display().to_string()));
            }
        }

        info!("Loading native plugin from {:?}", canonical);

        let library = unsafe {
            Library::new(&canonical)
                .map_err(|e| PluginError::LoadFailed(format!("Failed to load library: {}", e)))?
        };

        self.verify_api_version(&library)?;

        let instance = self.create_plugin_instance(&library)?;
        let id = next_plugin_id();

        let plugin_type = self.detect_plugin_type(&*instance);

        debug!(
            "Loaded plugin '{}' v{} as {:?}",
            instance.name(),
            instance.version(),
            plugin_type
        );

        self.plugins.insert(
            id,
            LoadedPlugin {
                id,
                path: canonical,
                library,
                instance,
                plugin_type,
            },
        );

        Ok(id)
    }

    pub fn unload(&mut self, id: PluginId) -> Result<()> {
        let mut plugin = self
            .plugins
            .remove(&id)
            .ok_or_else(|| PluginError::NotFound(format!("Plugin {:?}", id)))?;

        info!("Unloading plugin '{}'", plugin.instance.name());

        plugin.instance.shutdown()?;

        drop(plugin.instance);
        drop(plugin.library);

        Ok(())
    }

    pub fn reload(&mut self, id: PluginId) -> Result<()> {
        let plugin = self
            .plugins
            .get(&id)
            .ok_or_else(|| PluginError::NotFound(format!("Plugin {:?}", id)))?;

        let path = plugin.path.clone();

        info!("Hot-reloading plugin from {:?}", path);

        self.unload(id)?;

        let new_id = self.load(&path)?;

        if let Some(plugin) = self.plugins.remove(&new_id) {
            self.plugins.insert(id, LoadedPlugin { id, ..plugin });
        }

        Ok(())
    }

    pub fn get(&self, id: PluginId) -> Option<&dyn Plugin> {
        self.plugins.get(&id).map(|p| &*p.instance as &dyn Plugin)
    }

    pub fn get_mut(&mut self, id: PluginId) -> Option<&mut dyn Plugin> {
        self.plugins
            .get_mut(&id)
            .map(|p| &mut *p.instance as &mut dyn Plugin)
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins
            .values()
            .map(|p| PluginInfo {
                id: p.id,
                name: p.instance.name().to_string(),
                version: p.instance.version(),
                description: p.instance.description().to_string(),
                plugin_type: p.plugin_type,
            })
            .collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<PluginId> {
        self.plugins
            .iter()
            .find(|(_, p)| p.instance.name() == name)
            .map(|(id, _)| *id)
    }

    pub fn discover(&mut self) -> Result<Vec<PluginId>> {
        let mut loaded = Vec::new();

        for search_path in self.search_paths.clone() {
            if !search_path.exists() {
                continue;
            }

            let entries = std::fs::read_dir(&search_path).map_err(|e| {
                PluginError::LoadFailed(format!(
                    "Cannot read plugin directory {}: {}",
                    search_path.display(),
                    e
                ))
            })?;

            for entry in entries.flatten() {
                let path = entry.path();

                if self.is_plugin_library(&path) {
                    match self.load(&path) {
                        Ok(id) => loaded.push(id),
                        Err(e) => {
                            warn!("Failed to load plugin {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        Ok(loaded)
    }

    fn verify_api_version(&self, library: &Library) -> Result<()> {
        let version_fn: Symbol<unsafe extern "C" fn() -> *const std::ffi::c_char> = unsafe {
            library.get(b"musicfs_plugin_api_version").map_err(|_| {
                PluginError::SymbolNotFound("musicfs_plugin_api_version".to_string())
            })?
        };

        let version_ptr = unsafe { version_fn() };
        let version_str = unsafe { CStr::from_ptr(version_ptr) }
            .to_str()
            .map_err(|_| PluginError::VersionMismatch {
                expected: PLUGIN_API_VERSION.to_string(),
                actual: "<invalid UTF-8>".to_string(),
            })?;

        let plugin_version =
            Version::parse(version_str).map_err(|_| PluginError::VersionMismatch {
                expected: PLUGIN_API_VERSION.to_string(),
                actual: version_str.to_string(),
            })?;

        let expected_version = Version::parse(PLUGIN_API_VERSION).unwrap();

        if plugin_version.major != expected_version.major {
            return Err(PluginError::VersionMismatch {
                expected: PLUGIN_API_VERSION.to_string(),
                actual: version_str.to_string(),
            });
        }

        Ok(())
    }

    fn create_plugin_instance(&self, library: &Library) -> Result<Box<dyn Plugin>> {
        let create_fn: Symbol<unsafe extern "C" fn() -> *mut dyn Plugin> = unsafe {
            library
                .get(b"musicfs_plugin_create")
                .map_err(|_| PluginError::SymbolNotFound("musicfs_plugin_create".to_string()))?
        };

        let plugin_ptr = unsafe { create_fn() };
        if plugin_ptr.is_null() {
            return Err(PluginError::LoadFailed(
                "Plugin factory returned null".to_string(),
            ));
        }

        let plugin = unsafe { Box::from_raw(plugin_ptr) };
        Ok(plugin)
    }

    fn detect_plugin_type(&self, plugin: &dyn Plugin) -> PluginType {
        plugin.plugin_type()
    }

    fn is_plugin_library(&self, path: &Path) -> bool {
        let extension = path.extension().and_then(|e| e.to_str());

        match extension {
            Some("so") => true,
            Some("dylib") => true,
            Some("dll") => true,
            _ => false,
        }
    }
}

impl Default for NativePluginHost {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_host_creation() {
        let host = NativePluginHost::new();
        assert!(host.plugins.is_empty());
        assert!(host.search_paths.is_empty());
    }

    #[test]
    fn test_add_search_path() {
        let mut host = NativePluginHost::new();
        host.add_search_path(PathBuf::from("/usr/lib/musicfs/plugins"));
        host.add_search_path(PathBuf::from("/usr/lib/musicfs/plugins"));

        assert_eq!(host.search_paths.len(), 1);
    }

    #[test]
    fn test_is_plugin_library() {
        let host = NativePluginHost::new();

        assert!(host.is_plugin_library(Path::new("plugin.so")));
        assert!(host.is_plugin_library(Path::new("plugin.dylib")));
        assert!(host.is_plugin_library(Path::new("plugin.dll")));
        assert!(!host.is_plugin_library(Path::new("plugin.txt")));
        assert!(!host.is_plugin_library(Path::new("plugin")));
    }

    #[test]
    fn test_load_nonexistent() {
        let mut host = NativePluginHost::new();
        let result = host.load(Path::new("/nonexistent/plugin.so"));
        assert!(result.is_err());
    }
}
