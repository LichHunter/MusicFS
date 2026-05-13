# Week 10: Plugin System

**Phase**: 4 - Plugin System & Polish  
**Goal**: Extensibility via native and WASM plugins  
**Requirements**: FR-23.1-23.5, FR-24.1-24.3

---

## Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Plugin traits | musicfs-plugins | `traits.rs` | FR-23.1-23.4 |
| Native host | musicfs-plugins | `native.rs` | FR-23.2 |
| WASM host | musicfs-plugins | `wasm.rs` | FR-23.3 |
| Plugin lifecycle | musicfs-plugins | `manager.rs` | FR-23.5 |
| Example plugins | plugins/ | `example-origin/`, `example-format/` | FR-23.5 |

---

## Plugin Traits (`musicfs-plugins/src/traits.rs`)

```rust
/// Base plugin interface
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> Version;
    fn init(&mut self, config: Value) -> Result<(), PluginError>;
    fn shutdown(&mut self) -> Result<(), PluginError>;
}

/// Origin plugin interface (per architecture 4.3.4)
pub trait OriginPlugin: Plugin {
    fn origin_type(&self) -> &str;
    fn create(&self, config: Value) -> Result<Box<dyn Origin>, PluginError>;
}

/// Metadata source plugin
pub trait MetadataPlugin: Plugin {
    fn lookup(&self, query: &MetadataQuery) -> Result<Option<ExternalMetadata>, PluginError>;
}

/// Format plugin for custom audio formats (FR-24.1)
pub trait FormatPlugin: Plugin {
    fn extensions(&self) -> &[&str];
    fn can_handle(&self, extension: &str) -> bool;
    fn parse(&self, reader: &mut dyn Read) -> Result<AudioMeta, PluginError>;
}
```

---

## Native Plugin Host (`musicfs-plugins/src/native.rs`)

```rust
pub struct NativePluginHost {
    plugins: HashMap<String, LoadedPlugin>,
    search_paths: Vec<PathBuf>,
}

struct LoadedPlugin {
    library: libloading::Library,
    instance: Box<dyn Plugin>,
}

impl NativePluginHost {
    pub fn new() -> Self;
    
    /// Load plugin from shared library (.so/.dylib)
    pub fn load(&mut self, path: &Path) -> Result<PluginId, PluginError>;
    
    /// Unload plugin (FR-23.5)
    pub fn unload(&mut self, id: PluginId) -> Result<(), PluginError>;
    
    /// Hot reload plugin without restart (FR-23.4)
    pub fn reload(&mut self, id: PluginId) -> Result<(), PluginError>;
    
    /// List loaded plugins
    pub fn list(&self) -> Vec<PluginInfo>;
}
```

---

## WASM Plugin Host (`musicfs-plugins/src/wasm.rs`)

```rust
pub struct WasmPluginHost {
    engine: wasmtime::Engine,
    linker: wasmtime::Linker<PluginState>,
}

impl WasmPluginHost {
    pub fn new() -> Result<Self, PluginError>;
    
    /// Load WASM plugin with sandboxing (FR-23.3)
    pub fn load(&mut self, wasm_bytes: &[u8]) -> Result<WasmPlugin, PluginError>;
    
    /// Resource limits for sandboxed execution
    pub fn set_limits(&mut self, limits: ResourceLimits);
}

pub struct ResourceLimits {
    pub max_memory_mb: u32,
    pub max_cpu_time_ms: u32,
    pub allow_network: bool,
    pub allow_filesystem: bool,
}
```

---

## Plugin Manager (`musicfs-plugins/src/manager.rs`)

```rust
pub struct PluginManager {
    native_host: NativePluginHost,
    wasm_host: WasmPluginHost,
    registry: PluginRegistry,
}

impl PluginManager {
    /// Initialize and load plugins from config
    pub fn init(config: &PluginConfig) -> Result<Self, PluginError>;
    
    /// Get all origin plugins
    pub fn origin_plugins(&self) -> Vec<&dyn OriginPlugin>;
    
    /// Get all format plugins
    pub fn format_plugins(&self) -> Vec<&dyn FormatPlugin>;
    
    /// Get all metadata plugins
    pub fn metadata_plugins(&self) -> Vec<&dyn MetadataPlugin>;
    
    /// Reload all plugins (hot reload)
    pub fn reload_all(&mut self) -> Result<(), PluginError>;
}
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_native_plugin_load` | Unit | Native plugin loading (FR-23.2) |
| `test_native_plugin_unload` | Unit | Clean unload |
| `test_wasm_plugin_sandbox` | Unit | WASM isolation (FR-23.3) |
| `test_wasm_resource_limits` | Unit | Memory/CPU limits enforced |
| `test_plugin_hot_reload` | Integration | Reload without restart (FR-23.4) |
| `test_example_origin_plugin` | Integration | Custom origin works |
| `test_example_format_plugin` | Integration | Custom format works |

---

## Exit Criteria

- [ ] Native plugins loadable at runtime
- [ ] WASM plugins sandboxed with resource limits
- [ ] Example plugins functional
- [ ] Plugins hot-reloadable without daemon restart
- [ ] Plugin lifecycle management (load, unload, reload)

---

## Architecture Alignment

Per architecture.md section 4.3.4:
- Plugin loading: Built-in → Native (.so) → WASM
- Origin plugins create `Box<dyn Origin>`
- Format plugins register file extensions
- WASM runs in wasmtime sandbox

Per requirements.md:
- FR-23.1: Loadable plugins ✓
- FR-23.2: Stable plugin API ✓
- FR-23.3: Plugins for origins, metadata, formats ✓
- FR-23.4: WASM sandbox ✓
- FR-23.5: Plugin lifecycle ✓
