use crate::error::{PluginError, Result};
use crate::traits::PluginId;

#[cfg(feature = "wasm")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "wasm")]
static NEXT_WASM_PLUGIN_ID: AtomicU64 = AtomicU64::new(1_000_000);

#[cfg(feature = "wasm")]
fn next_wasm_plugin_id() -> PluginId {
    PluginId::new(NEXT_WASM_PLUGIN_ID.fetch_add(1, Ordering::SeqCst))
}

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_memory_mb: u32,
    pub max_cpu_time_ms: u32,
    pub allow_network: bool,
    pub allow_filesystem: bool,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_mb: 64,
            max_cpu_time_ms: 5000,
            allow_network: false,
            allow_filesystem: false,
        }
    }
}

#[cfg(feature = "wasm")]
mod wasm_impl {
    use super::*;
    use std::collections::HashMap;
    use tracing::info;
    use wasmtime::{Config, Engine, Linker, Module, Store};

    pub struct PluginState {
        limits: ResourceLimits,
    }

    pub struct WasmPlugin {
        id: PluginId,
        name: String,
        _module: Module,
    }

    impl WasmPlugin {
        pub fn id(&self) -> PluginId {
            self.id
        }

        pub fn name(&self) -> &str {
            &self.name
        }
    }

    pub struct WasmPluginHost {
        engine: Engine,
        linker: Linker<PluginState>,
        plugins: HashMap<PluginId, WasmPlugin>,
        limits: ResourceLimits,
    }

    impl WasmPluginHost {
        pub fn new() -> Result<Self> {
            let mut config = Config::new();
            config.consume_fuel(true);
            config.epoch_interruption(true);

            let engine = Engine::new(&config)
                .map_err(|e| PluginError::Wasm(format!("Failed to create WASM engine: {}", e)))?;

            let linker = Linker::new(&engine);

            Ok(Self {
                engine,
                linker,
                plugins: HashMap::new(),
                limits: ResourceLimits::default(),
            })
        }

        pub fn set_limits(&mut self, limits: ResourceLimits) {
            self.limits = limits;
        }

        pub fn load(&mut self, wasm_bytes: &[u8]) -> Result<PluginId> {
            info!("Loading WASM plugin ({} bytes)", wasm_bytes.len());

            let module = Module::new(&self.engine, wasm_bytes)
                .map_err(|e| PluginError::Wasm(format!("Failed to compile WASM module: {}", e)))?;

            let id = next_wasm_plugin_id();
            let name = module.name().unwrap_or("unnamed").to_string();

            let plugin = WasmPlugin {
                id,
                name,
                _module: module,
            };

            self.plugins.insert(id, plugin);

            Ok(id)
        }

        pub fn unload(&mut self, id: PluginId) -> Result<()> {
            self.plugins
                .remove(&id)
                .ok_or_else(|| PluginError::NotFound(format!("WASM plugin {:?}", id)))?;
            Ok(())
        }

        pub fn get(&self, id: PluginId) -> Option<&WasmPlugin> {
            self.plugins.get(&id)
        }

        pub fn list(&self) -> Vec<(PluginId, &str)> {
            self.plugins.iter().map(|(id, p)| (*id, p.name())).collect()
        }

        fn create_store(&self) -> Store<PluginState> {
            let state = PluginState {
                limits: self.limits.clone(),
            };

            let mut store = Store::new(&self.engine, state);

            let fuel = (self.limits.max_cpu_time_ms as u64) * 1_000_000;
            store.set_fuel(fuel).ok();

            store
        }
    }

    impl Default for WasmPluginHost {
        fn default() -> Self {
            Self::new().expect("Failed to create WASM host")
        }
    }
}

#[cfg(not(feature = "wasm"))]
mod wasm_stub {
    use super::*;

    pub struct WasmPluginHost {
        limits: ResourceLimits,
    }

    impl WasmPluginHost {
        pub fn new() -> Result<Self> {
            Ok(Self {
                limits: ResourceLimits::default(),
            })
        }

        pub fn set_limits(&mut self, limits: ResourceLimits) {
            self.limits = limits;
        }

        pub fn load(&mut self, _wasm_bytes: &[u8]) -> Result<PluginId> {
            Err(PluginError::Wasm(
                "WASM support not enabled. Compile with --features wasm".to_string(),
            ))
        }

        pub fn unload(&mut self, _id: PluginId) -> Result<()> {
            Err(PluginError::Wasm("WASM support not enabled".to_string()))
        }

        pub fn list(&self) -> Vec<(PluginId, &str)> {
            Vec::new()
        }
    }

    impl Default for WasmPluginHost {
        fn default() -> Self {
            Self::new().expect("Failed to create WASM stub host")
        }
    }
}

#[cfg(feature = "wasm")]
pub use wasm_impl::*;

#[cfg(not(feature = "wasm"))]
pub use wasm_stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_limits_default() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_memory_mb, 64);
        assert_eq!(limits.max_cpu_time_ms, 5000);
        assert!(!limits.allow_network);
        assert!(!limits.allow_filesystem);
    }

    #[test]
    fn test_wasm_host_creation() {
        let host = WasmPluginHost::new();
        assert!(host.is_ok());
    }

    #[test]
    #[cfg(not(feature = "wasm"))]
    fn test_wasm_disabled_load_fails() {
        let mut host = WasmPluginHost::new().unwrap();
        let result = host.load(&[0x00, 0x61, 0x73, 0x6d]);
        assert!(result.is_err());
    }
}
