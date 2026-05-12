# beetfs Data Flow

## Overview

This document details the complete data flow for read and write operations in beetfs.

---

## 1. Initialization Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           beet mount /mountpoint                            │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              mount(lib, config, opts, args)                  │
│                                                                             │
│  1. Parse PATH_FORMAT into structure_split                                  │
│     PATH_FORMAT = "$artist/$album ($year) [$format_upper]/..."              │
│     structure_split = ["$artist", "$album ($year) [$format_upper]", ...]   │
│     structure_depth = 3                                                     │
│                                                                             │
│  2. Store global library reference                                          │
│     library = lib                                                           │
│                                                                             │
│  3. Create empty virtual directory tree                                     │
│     directory_structure = FSNode({}, {})                                    │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         for item in lib.items():                            │
│                                                                             │
│  For each item in beets library:                                            │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  1. Build template mapping                                             │ │
│  │     mapping = {                                                        │ │
│  │       'artist': 'Pink Floyd',                                          │ │
│  │       'album': 'The Wall',                                             │ │
│  │       'year': '1979',                                                  │ │
│  │       'format_upper': 'FLAC',                                          │ │
│  │       'track': '01',                                                   │ │
│  │       'title': 'In The Flesh?',                                        │ │
│  │     }                                                                  │ │
│  │                                                                        │ │
│  │  2. Substitute template for each level                                 │ │
│  │     level_subbed[0] = "Pink Floyd"                                     │ │
│  │     level_subbed[1] = "The Wall (1979) [FLAC]"                         │ │
│  │     level_subbed[2] = "01 - Pink Floyd - In The Flesh?.flac"           │ │
│  │                                                                        │ │
│  │  3. Add directories to tree                                            │ │
│  │     directory_structure.adddir([], "Pink Floyd")                       │ │
│  │     directory_structure.adddir(["Pink Floyd"], "The Wall (1979)...")   │ │
│  │                                                                        │ │
│  │  4. Add file entry (filename → item.id)                                │ │
│  │     directory_structure.addfile(                                       │ │
│  │       ["Pink Floyd", "The Wall (1979) [FLAC]"],                        │ │
│  │       "01 - Pink Floyd - In The Flesh?.flac",                          │ │
│  │       item.id  # e.g., 42                                              │ │
│  │     )                                                                  │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     beetFileSystem FUSE Server                              │
│                                                                             │
│  server = beetFileSystem(...)                                               │
│  server.multithreaded = 0                                                   │
│  server.main()  ← Enters FUSE event loop                                    │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. File Open Flow

```
Application: open("/mount/Pink Floyd/The Wall (1979) [FLAC]/01 - Pink Floyd - In The Flesh?.flac")
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    beetFileSystem.open(path, flags)                          │
│                           Lines 988-1021                                     │
│                                                                             │
│  path = "/Pink Floyd/The Wall (1979) [FLAC]/01 - Pink Floyd - In The..."   │
│  flags = os.O_RDONLY (or O_RDWR)                                            │
│                                                                             │
│  if path in self.files:                                                     │
│      # File already open - increment reference count                        │
│      self.files[path].open()                                                │
│      return self.files[path]                                                │
│  else:                                                                      │
│      # Create new FileHandler                                               │
│      self.files[path] = FileHandler(path, self.lib)                         │
│      return self.files[path]                                                │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     FileHandler.__init__(path, lib)                          │
│                           Lines 440-483                                      │
│                                                                             │
│  Step 1: Resolve virtual path to beets item                                 │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  pathsplit = ["Pink Floyd", "The Wall (1979) [FLAC]",                  │ │
│  │               "01 - Pink Floyd - In The Flesh?.flac"]                  │ │
│  │                                                                        │ │
│  │  # Navigate to parent directory in virtual tree                        │ │
│  │  node = directory_structure.getnode(pathsplit[0:2])                    │ │
│  │  # node.files = {"01 - Pink Floyd - In The Flesh?.flac": 42, ...}      │ │
│  │                                                                        │ │
│  │  # Get beets item by ID                                                │ │
│  │  item_id = node.files[pathsplit[2]]  # 42                              │ │
│  │  self.item = lib.get_item(id=42)                                       │ │
│  │  self.real_path = self.item.path                                       │ │
│  │  # e.g., "/mnt/music/torrents/pink_floyd_wall.flac"                    │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 2: Open real file and detect format                                   │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.file_object = open(self.real_path, 'r+')                         │ │
│  │  self.format = "flac"  # from file extension                           │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 3: Create InterpolatedFLAC with database metadata                     │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.inf = InterpolatedFLAC(self.file_object.read())                  │ │
│  │                                                                        │ │
│  │  # INJECT DATABASE METADATA (this is the key operation!)               │ │
│  │  self.inf["title"] = self.item.title   # "In The Flesh?"               │ │
│  │  self.inf["album"] = self.item.album   # "The Wall"                    │ │
│  │  self.inf["artist"] = self.item.artist # "Pink Floyd"                  │ │
│  │  self.inf["genre"] = self.item.genre   # "Progressive Rock"            │ │
│  │                                                                        │ │
│  │  # Generate header with injected metadata                              │ │
│  │  self.header = self.inf.get_header(self.real_path)                     │ │
│  │  self.bound = len(self.header)  # e.g., 8192 bytes                     │ │
│  │  self.music_offset = self.inf.offset()                                 │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 4: Cache audio data                                                   │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.file_object.seek(self.music_offset)                              │ │
│  │  self.music_data = self.file_object.read()  # All audio data           │ │
│  │  self.file_object.close()                                              │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. File Read Flow

```
Application: read(fd, buffer, 4096)  # offset managed by kernel
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│              beetFileSystem.read(path, size, offset, fh)                     │
│                           Lines 1077-1106                                    │
│                                                                             │
│  path = "/Pink Floyd/The Wall (1979) [FLAC]/01 - ..."                       │
│  size = 4096                                                                │
│  offset = 0 (first read) or previous offset + bytes_read                    │
│  fh = FileHandler instance                                                  │
│                                                                             │
│  return self.files[path].read(size, offset)                                 │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    FileHandler.read(size, offset)                            │
│                           Lines 497-517                                      │
│                                                                             │
│  Variables:                                                                 │
│    self.bound = 8192      (header size)                                     │
│    self.header = bytes    (generated FLAC header with DB metadata)          │
│    self.music_data = bytes (original audio frames)                          │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
              ┌───────────────────────┼───────────────────────┐
              │                       │                       │
              ▼                       ▼                       ▼
┌─────────────────────┐ ┌─────────────────────┐ ┌─────────────────────┐
│ Case 1: Header Only │ │ Case 2: Span Both   │ │ Case 3: Audio Only  │
│ offset < bound      │ │ offset < bound      │ │ offset >= bound     │
│ offset+size < bound │ │ offset+size >= bound│ │                     │
├─────────────────────┤ ├─────────────────────┤ ├─────────────────────┤
│ Example:            │ │ Example:            │ │ Example:            │
│   offset=0          │ │   offset=8000       │ │   offset=10000      │
│   size=4096         │ │   size=4096         │ │   size=4096         │
│   bound=8192        │ │   bound=8192        │ │   bound=8192        │
├─────────────────────┤ ├─────────────────────┤ ├─────────────────────┤
│ Return:             │ │ Return:             │ │ Return:             │
│ header[0:4096]      │ │ header[8000:8192]   │ │ music_data[         │
│                     │ │ + music_data[0:3904]│ │   1808:5904]        │
│ (DB metadata!)      │ │                     │ │                     │
│                     │ │ (mixed)             │ │ (original audio)    │
└─────────────────────┘ └─────────────────────┘ └─────────────────────┘


Visual representation of virtual file:

  0                    bound (8192)                                   EOF
  │                       │                                            │
  ▼                       ▼                                            ▼
  ┌───────────────────────┬────────────────────────────────────────────┐
  │      HEADER           │              AUDIO DATA                    │
  │   (self.header)       │           (self.music_data)                │
  │                       │                                            │
  │  Contains:            │  Contains:                                 │
  │  - "fLaC" magic       │  - Original FLAC frames                    │
  │  - STREAMINFO block   │  - Unchanged from disk                     │
  │  - VORBIS_COMMENT     │                                            │
  │    with DB values:    │                                            │
  │    title, artist,     │                                            │
  │    album, genre       │                                            │
  │  - PADDING block      │                                            │
  └───────────────────────┴────────────────────────────────────────────┘
          ▲                              ▲
          │                              │
    From InterpolatedFLAC          From original file
    with injected DB tags          (passed through)
```

---

## 4. File Write Flow

```
Application: write(fd, "TITLE=New Title\0", 16)  # Hypothetical tag edit
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│              beetFileSystem.write(path, buf, offset, fh)                     │
│                           Lines 1108-1135                                    │
│                                                                             │
│  return self.files[path].write(offset, buf)                                 │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    FileHandler.write(offset, buf)                            │
│                           Lines 519-565                                      │
│                                                                             │
│  if offset >= self.bound:                                                   │
│      # Write is in audio area - DISCARD                                     │
│      return  # Do nothing, audio is read-only                               │
│                                                                             │
│  # Write is in header area - process tag update                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  Step 1: Reconstruct full virtual file in memory                            │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  filedata = self.header + self.music_data                              │ │
│  │                                                                        │ │
│  │  # Patch in new data                                                   │ │
│  │  filedata = filedata[0:offset] + buf + filedata[offset + len(buf):]    │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 2: Parse patched data as FLAC                                         │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.inf = InterpolatedFLAC(filedata)                                 │ │
│  │  # This parses the FLAC structure and extracts Vorbis comments         │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 3: Extract tag values from parsed FLAC                                │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.item.title = str(self.inf["title"][0]).encode('utf-8')           │ │
│  │  self.item.album = str(self.inf["album"][0]).encode('utf-8')           │ │
│  │  self.item.artist = str(self.inf["artist"][0]).encode('utf-8')         │ │
│  │  self.item.genre = str(self.inf["genre"][0]).encode('utf-8')           │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 4: Save to beets database                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.lib.store(self.item)  # Update item in library                   │ │
│  │  self.lib.save()            # Persist to SQLite                        │ │
│  │                                                                        │ │
│  │  # NOTE: Original file on disk is NEVER touched!                       │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  Step 5: Regenerate header for subsequent reads                             │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │  self.inf["title"] = self.item.title                                   │ │
│  │  self.inf["album"] = self.item.album                                   │ │
│  │  self.inf["artist"] = self.item.artist                                 │ │
│  │  self.inf["genre"] = self.item.genre                                   │ │
│  │                                                                        │ │
│  │  self.header = self.inf.get_header(self.real_path)                     │ │
│  │  self.bound = len(self.header)                                         │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  return len(buf)  # Success                                                 │
└─────────────────────────────────────────────────────────────────────────────┘


Write data flow summary:

  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
  │ Application │     │   beetfs    │     │    Beets    │     │  Original   │
  │   writes    │────▶│   parses    │────▶│   database  │     │    file     │
  │  new tags   │     │  extracts   │     │   updated   │     │  UNTOUCHED  │
  └─────────────┘     └─────────────┘     └─────────────┘     └─────────────┘
```

---

## 5. File Release Flow

```
Application: close(fd)
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│              beetFileSystem.release(path, flags, fh)                         │
│                           Lines 1049-1059                                    │
│                                                                             │
│  if self.files[path].release():                                             │
│      # Reference count reached 0, clean up                                  │
│      del self.files[path]                                                   │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    FileHandler.release()                                     │
│                           Lines 489-495                                      │
│                                                                             │
│  self.instance_count -= 1                                                   │
│                                                                             │
│  if self.instance_count == 0:                                               │
│      return True   # OK to delete                                           │
│  else:                                                                      │
│      return False  # Still in use                                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 6. Directory Listing Flow

```
Application: ls /mount/Pink\ Floyd/
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│              beetFileSystem.readdir(path, offset, dh)                        │
│                           Lines 931-975                                      │
│                                                                             │
│  path = "/Pink Floyd"                                                       │
│  pathsplit = ["Pink Floyd"]                                                 │
│                                                                             │
│  yield fuse.Direntry(".")                                                   │
│  yield fuse.Direntry("..")                                                  │
│                                                                             │
│  # len(pathsplit) == 1, structure_depth - 1 == 2                            │
│  # So we're listing directories (albums), not files                         │
│                                                                             │
│  for dirname in directory_structure.listdir(pathsplit, True):               │
│      yield fuse.Direntry(dirname.encode('utf-8'))                           │
│      # "The Wall (1979) [FLAC]"                                             │
│      # "Animals (1977) [FLAC]"                                              │
│      # etc.                                                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 7. Complete Request Lifecycle

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                          COMPLETE LIFECYCLE                                   │
│                                                                              │
│  1. User mounts: beet mount /mnt/music                                       │
│     ├─ Build virtual tree from beets library                                 │
│     └─ Start FUSE event loop                                                 │
│                                                                              │
│  2. Application opens file: open("/mnt/music/Artist/Album/track.flac")       │
│     ├─ Resolve virtual path to beets item ID                                 │
│     ├─ Load original file into memory                                        │
│     ├─ Inject database metadata into FLAC structure                          │
│     ├─ Generate new header with DB tags                                      │
│     └─ Cache audio data                                                      │
│                                                                              │
│  3. Application reads file: read(fd, buf, 4096)                              │
│     ├─ If reading header region → return header (DB metadata)                │
│     ├─ If reading audio region → return cached audio (original)              │
│     └─ If spanning both → return combined data                               │
│                                                                              │
│  4. Application writes tags: write(fd, new_tags, offset)                     │
│     ├─ If audio region → discard (read-only)                                 │
│     ├─ If header region:                                                     │
│     │   ├─ Parse new tag values                                              │
│     │   ├─ Update beets database                                             │
│     │   └─ Regenerate header                                                 │
│     └─ Original file NEVER modified                                          │
│                                                                              │
│  5. Application closes file: close(fd)                                       │
│     ├─ Decrement reference count                                             │
│     └─ Clean up if count == 0                                                │
│                                                                              │
│  6. User unmounts: fusermount -u /mnt/music                                  │
│     └─ fsdestroy() called, cleanup                                           │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```
