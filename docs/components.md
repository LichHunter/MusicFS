# beetfs Components Deep Dive

## Component Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            beetFs.py                                     │
│  ┌─────────────────────────────────────────────────────────────────────┐│
│  │                         PLUGIN LAYER                                 ││
│  │   beetFs (BeetsPlugin)          beetFs_command (Subcommand)         ││
│  │   mount()                       template_mapping()                   ││
│  └─────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────┐│
│  │                      VIRTUAL FILESYSTEM                              ││
│  │   FSNode                        beetFileSystem (fuse.Fuse)          ││
│  │   Stat                                                               ││
│  └─────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────┐│
│  │                     METADATA INTERPOLATION                           ││
│  │   FileHandler                   InterpolatedFLAC                     ││
│  │                                 InterpolatedID3                      ││
│  └─────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 1. Plugin Layer

### 1.1 beetFs (BeetsPlugin)

**Location**: Lines 188-191

```python
class beetFs(BeetsPlugin):
    """ The beets plugin hook."""
    def commands(self):
        return [beetFs_command]
```

**Purpose**: Registers beetfs as a beets plugin, exposing the `mount` subcommand.

### 1.2 beetFs_command

**Location**: Lines 47, 185

```python
beetFs_command = Subcommand('mount', help='Mount a beets filesystem')
beetFs_command.func = mount
```

**Purpose**: CLI subcommand definition for `beet mount`.

### 1.3 mount() Function

**Location**: Lines 119-183

```python
def mount(lib, config, opts, args):
    # 1. Validate arguments
    if not args:
        raise beets.ui.UserError('no mountpoint specified')
    
    # 2. Parse path template
    global structure_split
    structure_split = PATH_FORMAT.split("/")
    global structure_depth
    structure_depth = len(structure_split)
    
    # 3. Store library reference
    global library
    library = lib
    
    # 4. Build virtual directory tree
    global directory_structure
    directory_structure = FSNode({}, {})
    
    # 5. Iterate all library items
    for item in lib.items():
        mapping = template_mapping(lib, item)
        # ... build tree ...
        directory_structure.addfile(sub_elements, filename, item.id)
    
    # 6. Create and run FUSE server
    server = beetFileSystem(...)
    server.main()
```

**Key Variables Set**:
| Variable | Type | Purpose |
|----------|------|---------|
| `structure_split` | `List[str]` | Path template components |
| `structure_depth` | `int` | Number of path levels |
| `library` | `Library` | Beets library reference |
| `directory_structure` | `FSNode` | Root of virtual tree |

### 1.4 template_mapping() Function

**Location**: Lines 82-116

```python
def template_mapping(lib, item):
    """Builds a template substitution map from beets item."""
    mapping = {}
    for key in METADATA_KEYS:
        value = getattr(item, key)
        # Sanitize value for filesystem paths
        if isinstance(value, basestring):
            value = re.sub(r'[\\/:]|^\.', '_', value)
        elif key in ('track', 'tracktotal', 'disc', 'disctotal'):
            value = '%02i' % value  # Zero-pad numbers
        mapping[key] = value
    
    # Add format info
    format_ = os.path.splitext(item.path)[1][1:]
    mapping['format'] = format_
    mapping['format_upper'] = format_.upper()
    
    # Default values for missing fields
    if mapping['artist'] == '':
        mapping['artist'] = 'Unknown Artist'
    # ... etc
    
    return mapping
```

**Template Variables Available**:
| Variable | Source | Example |
|----------|--------|---------|
| `$artist` | `item.artist` | "Pink Floyd" |
| `$album` | `item.album` | "The Wall" |
| `$title` | `item.title` | "Comfortably Numb" |
| `$year` | `item.year` | "1979" |
| `$track` | `item.track` | "06" |
| `$format` | file extension | "flac" |
| `$format_upper` | file extension | "FLAC" |

---

## 2. Virtual Filesystem Layer

### 2.1 FSNode Class

**Location**: Lines 390-436

```python
class FSNode(object):
    """A directory node in the virtual filesystem tree."""
    
    def __init__(self, dirs, files):
        self.dirs = dirs    # Dict[str, FSNode] - subdirectories
        self.files = files  # Dict[str, int] - filename → beets item ID
```

**Methods**:

| Method | Purpose | Signature |
|--------|---------|-----------|
| `getnode()` | Navigate to nested node | `getnode(elements, root=None) → FSNode` |
| `adddir()` | Add a directory | `adddir(elements, directory, root=None)` |
| `addfile()` | Add a file entry | `addfile(elements, filename, id, root=None)` |
| `listdir()` | List contents | `listdir(elements, directories, root=None) → List[str]` |

**Example Tree Navigation**:
```python
# Path: /Artist/Album/track.flac
# structure_split = ["$artist", "$album ($year) [$format_upper]", "$track - $artist - $title.$format"]

elements = ["Artist", "Album (2020) [FLAC]"]
node = directory_structure.getnode(elements)
# node.files = {"01 - Artist - Track.flac": 42, ...}

item_id = node.files["01 - Artist - Track.flac"]
# item_id = 42
```

### 2.2 Stat Class

**Location**: Lines 568-619

```python
class Stat(fuse.Stat):
    DIRSIZE = 4096
    
    def __init__(self, st_mode, st_size, st_nlink=1, st_uid=None, st_gid=None,
                 dt_atime=None, dt_mtime=None, dt_ctime=None):
        self.st_mode = st_mode
        self.st_ino = 0
        self.st_dev = 0
        self.st_nlink = st_nlink
        self.st_uid = st_uid or os.getuid()
        self.st_gid = st_gid or os.getgid()
        self.st_size = st_size
        # ... timestamps ...
```

**Purpose**: Represents file/directory metadata for FUSE stat operations.

### 2.3 beetFileSystem Class

**Location**: Lines 622-1144

```python
class beetFileSystem(fuse.Fuse):
    """Main FUSE filesystem implementation."""
    
    def __init__(self, *args, **kwargs):
        logging.basicConfig(filename="LOG", level=logging.INFO)
        super(beetFileSystem, self).__init__(*args, **kwargs)
    
    def fsinit(self):
        """Called after filesystem is mounted."""
        self.lib = library
        self.files = {}  # Dict[path, FileHandler]
```

**FUSE Operations Implemented**:

| Operation | Lines | Purpose |
|-----------|-------|---------|
| `fsinit()` | 630-636 | Post-mount initialization |
| `fsdestroy()` | 638-639 | Pre-unmount cleanup |
| `statfs()` | 641-646 | Filesystem statistics |
| `getattr()` | 648-707 | Get file/dir attributes |
| `access()` | 723-756 | Check permissions |
| `readdir()` | 931-975 | List directory contents |
| `open()` | 988-1021 | Open file |
| `read()` | 1077-1106 | Read file data |
| `write()` | 1108-1135 | Write file data |
| `release()` | 1049-1059 | Close file |

**Not Implemented (return EOPNOTSUPP)**:
- `mknod()`, `mkdir()`, `unlink()`, `rmdir()`
- `symlink()`, `link()`, `rename()`
- `chmod()`, `chown()`, `truncate()`

---

## 3. Metadata Interpolation Layer

### 3.1 FileHandler Class

**Location**: Lines 439-565

This is the **core component** that implements metadata overlay.

```python
class FileHandler(object):
    def __init__(self, path, lib):
        self.path = path          # Virtual path
        self.lib = lib            # Beets library
        
        # Resolve virtual path to real file
        pathsplit = path[1:].split('/')
        self.item = self.lib.get_item(id=directory_structure
                                      .getnode(pathsplit[0:structure_depth-1])
                                      .files[pathsplit[structure_depth-1]])
        self.real_path = self.item.path
        
        # Open real file
        self.file_object = open(self.real_path, 'r+')
        self.instance_count = 1
        
        # Determine format
        self.format = os.path.splitext(path)[1][1:].lower()
        
        if self.format == "flac":
            # Load file into interpolated FLAC object
            self.inf = InterpolatedFLAC(self.file_object.read())
            
            # INJECT DATABASE METADATA
            self.inf["title"] = self.item.title
            self.inf["album"] = self.item.album
            self.inf["artist"] = self.item.artist
            self.inf["genre"] = self.item.genre
            
            # Generate new header with DB metadata
            self.header = self.inf.get_header(self.real_path)
            self.bound = len(self.header)
            self.music_offset = self.inf.offset()
        
        elif self.format == "mp3":
            self.bound = 0  # MP3 interpolation disabled
            self.music_offset = 0
        
        # Cache audio data
        self.file_object.seek(self.music_offset)
        self.music_data = self.file_object.read()
        self.file_object.close()
```

**Key Attributes**:

| Attribute | Type | Purpose |
|-----------|------|---------|
| `path` | `str` | Virtual path (e.g., `/Artist/Album/track.flac`) |
| `real_path` | `str` | Actual file path on disk |
| `item` | `Item` | Beets library item (has DB metadata) |
| `format` | `str` | File format ("flac", "mp3") |
| `inf` | `InterpolatedFLAC` | Mutagen object with injected metadata |
| `header` | `bytes` | Generated header with DB tags |
| `bound` | `int` | Byte offset where header ends |
| `music_offset` | `int` | Byte offset in original file where audio starts |
| `music_data` | `bytes` | Cached audio data |
| `instance_count` | `int` | Reference count for file handles |

### 3.2 FileHandler.read() Method

**Location**: Lines 497-517

```python
def read(self, size, offset):
    # Case 1: Reading within header boundary
    if offset < self.bound:
        if offset + size < len(self.header):
            # Entire read is within header
            return self.header[offset:offset+size]
        else:
            # Read spans header and audio
            ret = self.header[offset:len(self.header)]
            ret = ret + self.music_data[0:size - (len(self.header) - offset)]
            return ret
    
    # Case 2: Reading audio data only
    return self.music_data[offset - len(self.header):offset - len(self.header) + size]
```

**Read Logic Diagram**:

```
Virtual File Layout:
┌────────────────────────────────────────────────────────────────┐
│ 0        bound                                            EOF  │
│ ├─────────┼────────────────────────────────────────────────┤  │
│ │ HEADER  │              AUDIO DATA                        │  │
│ │ (from   │              (from self.music_data)            │  │
│ │  self.  │                                                │  │
│ │ header) │                                                │  │
│ └─────────┴────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘

Read scenarios:
1. offset=0, size=100, bound=500    → Return header[0:100]
2. offset=400, size=200, bound=500  → Return header[400:500] + music[0:100]
3. offset=600, size=100, bound=500  → Return music[100:200]
```

### 3.3 FileHandler.write() Method

**Location**: Lines 519-565

```python
def write(self, offset, buf):
    # Only handle writes to header area
    if offset < self.bound:
        # Reconstruct full file in memory
        filedata = self.header + self.music_data
        
        # Patch in new data
        filedata = filedata[0:offset] + buf + filedata[offset + len(buf):]
        
        if self.format == "flac":
            # Parse the patched data
            self.inf = InterpolatedFLAC(filedata)
            
            # EXTRACT new tag values and save to DB
            self.item.title = str(self.inf["title"][0]).encode('utf-8')
            self.item.album = str(self.inf["album"][0]).encode('utf-8')
            self.item.artist = str(self.inf["artist"][0]).encode('utf-8')
            self.item.genre = str(self.inf["genre"][0]).encode('utf-8')
            
            # Persist to beets database
            self.lib.store(self.item)
            self.lib.save()
            
            # Regenerate header with updated values
            self.inf["title"] = self.item.title
            self.inf["album"] = self.item.album
            self.inf["artist"] = self.item.artist
            self.inf["genre"] = self.item.genre
            
            self.header = self.inf.get_header(self.real_path)
            self.bound = len(self.header)
            
            return len(buf)
```

**Write Flow**:
```
1. App writes new tag data to header region
                    │
                    ▼
2. Patch header + music_data with new bytes
                    │
                    ▼
3. Parse patched data as FLAC
                    │
                    ▼
4. Extract tag values from parsed FLAC
                    │
                    ▼
5. Update beets Item with new values
                    │
                    ▼
6. lib.store(item) + lib.save() → SQLite
                    │
                    ▼
7. Regenerate header for subsequent reads
```

### 3.4 InterpolatedFLAC Class

**Location**: Lines 274-388

```python
class InterpolatedFLAC(FLAC):
    """Custom FLAC handler that can load from bytes and generate headers."""
    
    def load(self, filedata):
        """Load FLAC from byte string instead of file."""
        self.metadata_blocks = []
        self.tags = None
        self.filedata = filedata
        self.fileobj = BytesIO(filedata)
        self.__check_header(self.fileobj)
        
        while self.__read_metadata_block(self.fileobj):
            pass
        
        # Verify audio frame starts correctly
        if self.fileobj.read(2) not in ["\xff\xf8", "\xff\xf9"]:
            raise FLACNoHeaderError("End of metadata did not start audio")
    
    def get_header(self, filename=None):
        """Generate FLAC header with current metadata."""
        # Add padding block
        self.metadata_blocks.append(Padding('\x00' * 1020))
        MetadataBlock.group_padding(self.metadata_blocks)
        
        # Calculate available space
        header = self.__check_header(self.fileobj)
        available = self.__find_audio_offset(self.fileobj) - header
        data = MetadataBlock.writeblocks(self.metadata_blocks)
        
        # Adjust padding to match available space
        if len(data) > available:
            # Reduce padding
            padding = self.metadata_blocks[-1]
            padding.length -= (len(data) - available)
            data = MetadataBlock.writeblocks(self.metadata_blocks)
        elif len(data) < available:
            # Increase padding
            self.metadata_blocks[-1].length += (available - len(data))
            data = MetadataBlock.writeblocks(self.metadata_blocks)
        
        self.__offset = len("fLaC" + data)
        return "fLaC" + data
    
    def offset(self):
        """Return byte offset where audio data starts."""
        return self.__offset
```

**FLAC Structure**:
```
┌──────────────────────────────────────────────────────────────────┐
│ "fLaC" │ STREAMINFO │ VORBIS_COMMENT │ ... │ PADDING │ AUDIO... │
│  (4B)  │   block    │     block      │     │  block  │          │
└──────────────────────────────────────────────────────────────────┘
         │◄──────── metadata_blocks ─────────►│
         │                                     │
         └──── get_header() returns this ─────┘
```

### 3.5 InterpolatedID3 Class

**Location**: Lines 200-271

```python
class InterpolatedID3(ID3):
    """Custom ID3 handler for MP3 files."""
    
    def save(self, filename=None, v1=0):
        """Save ID3 tags to file."""
        # Sort frames by importance
        order = ["TIT2", "TPE1", "TRCK", "TALB", "TPOS", "TDRC", "TCON"]
        # ... write header ...
```

**Note**: MP3 support is **incomplete** in the current implementation. The `FileHandler.__init__` sets `self.bound = 0` for MP3, effectively disabling interpolation.

---

## 4. Supported Metadata Fields

**Location**: Lines 55-77

```python
METADATA_RW_FIELDS = [
    ('title',       'text'),
    ('artist',      'text'),
    ('album',       'text'),
    ('genre',       'text'),
    ('composer',    'text'),
    ('grouping',    'text'),
    ('year',        'int'),
    ('month',       'int'),
    ('day',         'int'),
    ('track',       'int'),
    ('tracktotal',  'int'),
    ('disc',        'int'),
    ('disctotal',   'int'),
    ('lyrics',      'text'),
    ('comments',    'text'),
    ('bpm',         'int'),
    ('comp',        'bool'),
]
```

**Actually Implemented** (in FileHandler):
| Field | Read | Write |
|-------|------|-------|
| `title` | ✅ | ✅ |
| `artist` | ✅ | ✅ |
| `album` | ✅ | ✅ |
| `genre` | ✅ | ✅ |
| Others | ❌ | ❌ |

---

## 5. Error Handling

**Error Codes Used**:

| Code | Constant | Usage |
|------|----------|-------|
| 2 | `ENOENT` | File/directory not found |
| 13 | `EACCES` | Permission denied |
| 1 | `EPERM` | Operation not permitted |
| 95 | `EOPNOTSUPP` | Operation not supported |

**Exception Handling Pattern**:
```python
def getattr(self, path):
    try:
        # ... logic ...
    except Exception as e:
        logging.error(e)
        return -errno.ENOENT
```
